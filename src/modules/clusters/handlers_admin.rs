// clusters — HTTP handlers: settings, domains, and hook handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use actix_web::{HttpRequest, HttpResponse, web};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Sync settings and the domain registry.
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    json::respond(Ok::<SyncSettings, UnifiedError>(state.settings()), debug)
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    pub(crate) replay_window_secs: Option<i64>,
    pub(crate) health_timeout_secs: Option<u64>,
    pub(crate) sync_timeout_secs: Option<u64>,
    pub(crate) health_failure_threshold: Option<u32>,
    pub(crate) max_sync_logs: Option<usize>,
    pub(crate) max_health_history: Option<usize>,
    pub(crate) max_conflicts: Option<usize>,
    pub(crate) max_retries: Option<usize>,
    pub(crate) max_idempotency_records: Option<usize>,
    pub(crate) retry_max_attempts: Option<u32>,
    pub(crate) retry_base_delay_secs: Option<u64>,
    pub(crate) retry_max_delay_secs: Option<u64>,
    pub(crate) retry_backoff_multiplier: Option<f64>,
    pub(crate) idempotency_ttl_secs: Option<i64>,
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncSettings> = (|| {
        let mut settings = state.settings();
        if let Some(value) = body.replay_window_secs {
            if !(30..=3600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "replay_window_secs must be between 30 and 3600",
                ));
            }
            settings.replay_window_secs = value;
        }
        if let Some(value) = body.health_timeout_secs {
            if !(1..=300).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "health_timeout_secs must be between 1 and 300",
                ));
            }
            settings.health_timeout_secs = value;
        }
        if let Some(value) = body.sync_timeout_secs {
            if !(1..=600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "sync_timeout_secs must be between 1 and 600",
                ));
            }
            settings.sync_timeout_secs = value;
        }
        if let Some(value) = body.health_failure_threshold {
            if !(1..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "health_failure_threshold must be between 1 and 100",
                ));
            }
            settings.health_failure_threshold = value;
        }
        for (name, value) in [
            ("max_sync_logs", body.max_sync_logs),
            ("max_health_history", body.max_health_history),
            ("max_conflicts", body.max_conflicts),
            ("max_retries", body.max_retries),
            ("max_idempotency_records", body.max_idempotency_records),
        ] {
            if let Some(value) = value {
                if !(10..=100_000).contains(&value) {
                    return Err(UnifiedError::bad_request(format!(
                        "{name} must be between 10 and 100000"
                    )));
                }
            }
        }
        if let Some(value) = body.max_sync_logs {
            settings.max_sync_logs = value;
        }
        if let Some(value) = body.max_health_history {
            settings.max_health_history = value;
        }
        if let Some(value) = body.max_conflicts {
            settings.max_conflicts = value;
        }
        if let Some(value) = body.max_retries {
            settings.max_retries = value;
        }
        if let Some(value) = body.max_idempotency_records {
            settings.max_idempotency_records = value;
        }
        if let Some(value) = body.retry_max_attempts {
            if !(1..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_max_attempts must be between 1 and 100",
                ));
            }
            settings.retry_max_attempts = value;
        }
        if let Some(value) = body.retry_base_delay_secs {
            if !(1..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_base_delay_secs must be between 1 and 86400",
                ));
            }
            settings.retry_base_delay_secs = value;
        }
        if let Some(value) = body.retry_max_delay_secs {
            if !(1..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_max_delay_secs must be between 1 and 86400",
                ));
            }
            settings.retry_max_delay_secs = value;
        }
        if settings.retry_max_delay_secs < settings.retry_base_delay_secs {
            return Err(UnifiedError::bad_request(
                "retry_max_delay_secs must not be smaller than retry_base_delay_secs",
            ));
        }
        if let Some(value) = body.retry_backoff_multiplier {
            if !value.is_finite() || !(1.0..=10.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_backoff_multiplier must be between 1.0 and 10.0",
                ));
            }
            settings.retry_backoff_multiplier = value;
        }
        if let Some(value) = body.idempotency_ttl_secs {
            if !(60..=604_800).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idempotency_ttl_secs must be between 60 and 604800",
                ));
            }
            settings.idempotency_ttl_secs = value;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            CLUSTER_SYNC_SETTINGS_UPDATED,
            json!({
                "replay_window_secs": settings.replay_window_secs,
                "health_timeout_secs": settings.health_timeout_secs,
                "sync_timeout_secs": settings.sync_timeout_secs,
                "health_failure_threshold": settings.health_failure_threshold,
                "retry_max_attempts": settings.retry_max_attempts,
                "retry_base_delay_secs": settings.retry_base_delay_secs,
                "retry_max_delay_secs": settings.retry_max_delay_secs,
                "retry_backoff_multiplier": settings.retry_backoff_multiplier,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_domains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let mut domains: Vec<String> = state
        .domains
        .read()
        .expect("domains lock poisoned")
        .iter()
        .cloned()
        .collect();
    domains.sort();
    json::respond(
        Ok::<Value, UnifiedError>(json!({ "domains": domains, "count": domains.len() })),
        debug,
    )
}

#[derive(Debug, Deserialize)]
struct RegisterDomainRequest {
    pub(crate) domain: String,
}

pub(crate) async fn register_domain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<RegisterDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let domain = body.domain.trim().to_string();
        if !valid_domain_name(&domain) {
            return Err(UnifiedError::bad_request(
                "domain must be 1-64 characters of lowercase letters, digits or underscore",
            ));
        }
        let mut domains = state.domains.write().expect("domains lock poisoned");
        if !domains.insert(domain.clone()) {
            return Err(UnifiedError::conflict(format!(
                "Sync domain {domain} is already registered"
            )));
        }
        let mut registered: Vec<String> = domains.iter().cloned().collect();
        registered.sort();
        Ok(json!({ "domain": domain, "domains": registered }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn unregister_domain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let domain = domain.into_inner();
        if !valid_domain_name(&domain) {
            return Err(UnifiedError::bad_request("Invalid sync domain name"));
        }
        let in_use = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .any(|plan| plan.domains.iter().any(|d| d == &domain));
        if in_use {
            return Err(UnifiedError::conflict(format!(
                "Sync domain {domain} is still used by a sync plan"
            )));
        }
        let mut domains = state.domains.write().expect("domains lock poisoned");
        if domains.len() <= 1 {
            return Err(UnifiedError::unprocessable(
                "At least one sync domain must stay registered",
            ));
        }
        if !domains.remove(&domain) {
            return Err(UnifiedError::not_found(format!(
                "Sync domain {domain} is not registered"
            )));
        }
        let mut registered: Vec<String> = domains.iter().cloned().collect();
        registered.sort();
        Ok(json!({ "deleted": true, "domain": domain, "domains": registered }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook listeners: scheduled health checks and scheduled sync runs.
// ---------------------------------------------------------------------------

pub(crate) fn on_health_check_requested(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let Some(node_id_text) = ctx.data.get("node_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "queued": false, "error": "cluster.health.check_requested requires 'node_id'" }));
        return;
    };
    let Ok(node_id) = node_id_text.parse::<Uuid>() else {
        ctx.add_result(json!({ "queued": false, "error": "node_id must be a UUID" }));
        return;
    };
    spawn_background(async move {
        let _ = probe_node(&state, node_id).await;
    });
    ctx.add_result(json!({ "queued": true, "node_id": node_id }));
}

pub(crate) fn on_sync_requested(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let Some(plan_id_text) = ctx.data.get("plan_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "queued": false, "error": "cluster.sync.requested requires 'plan_id'" }));
        return;
    };
    let Ok(plan_id) = plan_id_text.parse::<Uuid>() else {
        ctx.add_result(json!({ "queued": false, "error": "plan_id must be a UUID" }));
        return;
    };
    let dry_run = ctx
        .data
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    spawn_background(async move {
        let _ = execute_sync(&state, plan_id, dry_run).await;
    });
    ctx.add_result(json!({ "queued": true, "plan_id": plan_id, "dry_run": dry_run }));
}

pub(crate) fn on_sync_retry_due(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let state_clone = state.clone();
    spawn_background(async move {
        let _ = process_due_retries(&state_clone).await;
    });
    ctx.add_result(json!({ "queued": true }));
}
