//! incidents - handlers auto open.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookContext;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, module::*};



// Auto-open rules + Service Monitor listener
pub(crate) async fn list_auto_open(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let enabled_filter = params.filters.get("enabled").cloned();
        let mut items: Vec<AutoOpenRule> = state
            .auto_open_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if rule.enabled != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rule| rule.service_id.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn set_auto_open(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SetAutoOpenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AutoOpenRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let service_id = require_text(&path.into_inner(), "service_id", 255)?;
        let input = body.into_inner();
        let rule = AutoOpenRule {
            service_id: service_id.clone(),
            enabled: input.enabled,
            severity: input.severity.unwrap_or_default(),
            auto_resolve_on_recovery: input.auto_resolve_on_recovery,
            updated_at: Utc::now(),
        };
        state.auto_open_rules.insert(service_id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}


/// Service Monitor listener: `cluster.health.changed`. A `down`
/// health for a service with an enabled auto-open rule opens an
/// incident (deduplicated: no second open auto-incident for the
/// same service). A recovery appends a timeline note to open
/// auto-incidents for that service and resolves them when the rule
/// asks for it. Auto-open is off by default for every service.
pub(crate) fn on_service_health_changed(state: Arc<IncidentsState>, ctx: &mut HookContext) {
    let Some(service_id) = ctx.get("service_id").and_then(|value| value.as_str()) else {
        return;
    };
    let service_id = service_id.to_string();
    let health = ctx
        .get("health")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let service_name = ctx
        .get("service_name")
        .and_then(|value| value.as_str())
        .unwrap_or(&service_id)
        .to_string();
    if health == "down" {
        let Some(rule) = state
            .auto_open_rules
            .get(&service_id)
            .map(|entry| entry.value().clone())
        else {
            return;
        };
        if !rule.enabled {
            return;
        }
        // Deduplicate: an open auto-incident for this service
        // already exists.
        let already_open = state.incidents.iter().any(|entry| {
            let incident = entry.value();
            incident.source == "auto"
                && incident.source_service_id.as_deref() == Some(service_id.as_str())
                && !matches!(
                    incident.status,
                    IncidentStatus::Resolved | IncidentStatus::Closed
                )
        });
        if already_open {
            return;
        }
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title: format!("Service down: {service_name}"),
            description: ctx
                .get("detail")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            severity: rule.severity,
            status: IncidentStatus::Open,
            affected_service_ids: vec![service_id.clone()],
            responders: Vec::new(),
            current_responder: None,
            source: "auto".to_string(),
            source_service_id: Some(service_id.clone()),
            escalation_policy_id: None,
            rotation_id: None,
            created_by: "service-monitor".to_string(),
            acknowledged_by: None,
            acknowledged_at: None,
            resolved_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            "service-monitor",
            "system",
            format!("Auto-opened from Service Monitor alert for service {service_name}"),
        );
        state.emit(
            "incidents.created",
            json!({
                "incident_id": incident.id.to_string(),
                "title": incident.title,
                "severity": incident.severity.as_str(),
                "status": "open",
                "source": "auto",
                "source_service_id": service_id,
            }),
        );
        return;
    }
    // Recovery (any health other than "down"): note it on open
    // auto-incidents for the service, and resolve them when the
    // rule asks for it.
    let open_auto: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|incident| {
            incident.source == "auto"
                && incident.source_service_id.as_deref() == Some(service_id.as_str())
                && !matches!(
                    incident.status,
                    IncidentStatus::Resolved | IncidentStatus::Closed
                )
        })
        .collect();
    for mut incident in open_auto {
        state.add_timeline(
            incident.id,
            "service-monitor",
            "system",
            format!("Service {service_name} recovered (health: {health})"),
        );
        let rule = state
            .auto_open_rules
            .get(&service_id)
            .map(|entry| entry.value().clone());
        if rule.map(|rule| rule.auto_resolve_on_recovery).unwrap_or(false) {
            // Resolve through the machine: open -> acknowledged ->
            // investigating -> mitigating -> resolved. Acknowledge
            // and investigate first so the transition is valid.
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Acknowledged,
                "service-monitor",
                Some("Service recovered".to_string()),
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Investigating,
                "service-monitor",
                None,
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Mitigating,
                "service-monitor",
                None,
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Resolved,
                "service-monitor",
                Some(format!("Service {service_name} recovered")),
            );
        }
    }
}
