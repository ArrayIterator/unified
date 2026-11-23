//! incidents - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{ HttpRequest, HttpResponse };
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}


pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}


pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}



// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}


pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}


pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}


pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}


pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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


/// A handover time is "HH:MM" in 24-hour form.
pub(crate) fn valid_handover_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 2 || parts[0].len() != 2 || parts[1].len() != 2 {
        return false;
    }
    let Ok(hour) = parts[0].parse::<u32>() else {
        return false;
    };
    let Ok(minute) = parts[1].parse::<u32>() else {
        return false;
    };
    hour < 24 && minute < 60
}


/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
pub(crate) fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > 64 {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}


pub(crate) fn validate_user_ids(ids: &[String], field: &str, max: usize) -> UnifiedResult<Vec<String>> {
    if ids.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must contain at most {max} entries"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in ids {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "{field} must not contain empty entries"
            )));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}


pub(crate) fn validate_escalation_steps(steps: &[EscalationStep]) -> UnifiedResult<()> {
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(UnifiedError::bad_request(format!(
            "steps must contain between 1 and {MAX_STEPS} entries"
        )));
    }
    let mut previous_minutes: Option<i64> = None;
    for step in steps {
        if step.after_minutes < 0 || step.after_minutes > 60 * 24 * 30 {
            return Err(UnifiedError::bad_request(
                "steps[].after_minutes must be between 0 and 43200 (30 days)",
            ));
        }
        if let Some(previous) = previous_minutes {
            if step.after_minutes <= previous {
                return Err(UnifiedError::bad_request(
                    "steps[].after_minutes must strictly increase across steps",
                ));
            }
        }
        previous_minutes = Some(step.after_minutes);
        match step.target.as_str() {
            "responder" => {}
            "manager" => {
                if step.manager_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(UnifiedError::bad_request(
                        "steps[] with target \"manager\" requires manager_id",
                    ));
                }
            }
            "user" => {
                if step.user_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(UnifiedError::bad_request(
                        "steps[] with target \"user\" requires user_id",
                    ));
                }
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "steps[].target must be \"responder\", \"manager\", or \"user\", got \"{other}\""
                )))
            }
        }
    }
    Ok(())
}


pub(crate) fn load_incident(state: &IncidentsState, raw_id: &str) -> UnifiedResult<Incident> {
    let id = parse_uuid(raw_id, "incident")?;
    state
        .incidents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Incident not found: {id}")))
}


pub(crate) fn load_rotation(state: &IncidentsState, raw_id: &str) -> UnifiedResult<OnCallRotation> {
    let id = parse_uuid(raw_id, "rotation")?;
    state
        .rotations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rotation not found: {id}")))
}


pub(crate) fn load_policy(state: &IncidentsState, raw_id: &str) -> UnifiedResult<EscalationPolicy> {
    let id = parse_uuid(raw_id, "escalation policy")?;
    state
        .escalation_policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Escalation policy not found: {id}")))
}


pub(crate) fn timeline_of(state: &IncidentsState, incident_id: Uuid) -> Vec<IncidentTimelineEntry> {
    let mut items: Vec<IncidentTimelineEntry> = state
        .timeline
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.incident_id == incident_id)
        .collect();
    items.sort_by_key(|entry| entry.created_at);
    items
}


/// Who is on call for `rotation` at `now`: an `override` schedule
/// entry covering now wins; otherwise a `regular` entry covering
/// now wins; otherwise the rotation's `current_index` member (the
/// handover cycle). Returns `None` when the rotation has no
/// members.
pub(crate) fn on_call_member(state: &IncidentsState, rotation: &OnCallRotation, now: DateTime<Utc>) -> Option<String> {
    let mut covering: Vec<OnCallSchedule> = state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.rotation_id == rotation.id && entry.starts_at <= now && now < entry.ends_at
        })
        .collect();
    // Overrides first, then the latest-starting regular entry.
    covering.sort_by(|a, b| {
        let rank = |kind: &str| if kind == "override" { 0 } else { 1 };
        rank(&a.kind)
            .cmp(&rank(&b.kind))
            .then(b.starts_at.cmp(&a.starts_at))
    });
    if let Some(entry) = covering.first() {
        return Some(entry.member_id.clone());
    }
    rotation.members.get(rotation.current_index).cloned().or_else(|| {
        rotation.members.first().cloned()
    })
}


/// Advance a rotation to its next member (handover). The handover
/// time of day is honoured by the caller scheduling handovers; this
/// helper only moves the index and records the move.
pub(crate) fn advance_rotation(state: &IncidentsState, rotation: &mut OnCallRotation) {
    if rotation.members.is_empty() {
        return;
    }
    rotation.current_index = (rotation.current_index + 1) % rotation.members.len();
    rotation.updated_at = Utc::now();
    state.rotations.insert(rotation.id, rotation.clone());
}


/// Start escalation tracking for an incident: the first policy step
/// becomes due `after_minutes` after now. Only when the incident
/// has a policy and is still `open` or `acknowledged`.
pub(crate) fn start_escalation(state: &IncidentsState, incident: &Incident) {
    let Some(policy_id) = incident.escalation_policy_id else {
        return;
    };
    let Some(policy) = state
        .escalation_policies
        .get(&policy_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if policy.steps.is_empty() {
        return;
    }
    let first = &policy.steps[0];
    let now = Utc::now();
    let escalation = IncidentEscalation {
        id: Uuid::now_v7(),
        incident_id: incident.id,
        policy_id,
        step_index: 0,
        next_escalation_at: now + chrono::Duration::minutes(first.after_minutes),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    };
    state.escalations.insert(escalation.id, escalation);
}


pub(crate) fn stop_escalation(state: &IncidentsState, incident_id: Uuid) {
    for mut entry in state.escalations.iter_mut() {
        if entry.value().incident_id == incident_id && entry.value().status == "active" {
            entry.value_mut().status = "stopped".to_string();
            entry.value_mut().updated_at = Utc::now();
        }
    }
}


/// Page one user for an incident: record the attempt, write a
/// timeline entry, and emit a high-priority `notification.requested`.
/// Returns the recorded attempt.
pub(crate) fn page_user(
    state: &IncidentsState,
    incident: &Incident,
    user_id: &str,
    channel: &str,
) -> PageAttempt {
    let now = Utc::now();
    let attempt = PageAttempt {
        id: Uuid::now_v7(),
        incident_id: incident.id,
        paged_user_id: user_id.to_string(),
        channel: channel.to_string(),
        status: "sent".to_string(),
        acknowledged_at: None,
        created_at: now,
        updated_at: now,
    };
    state.page_attempts.insert(attempt.id, attempt.clone());
    state.add_timeline(
        incident.id,
        "system",
        "page",
        format!("Paged {user_id} via {channel} for incident \"{}\"", incident.title),
    );
    state.notify(
        "incidents.paged",
        user_id,
        &format!("Incident: {}", incident.title),
        &format!(
            "You have been paged for a {} severity incident: {}",
            incident.severity.as_str(),
            incident.title
        ),
        Some(format!("/api/v1/incidents/{}", incident.id)),
        "high",
    );
    state.emit(
        "incidents.paged",
        json!({
            "incident_id": incident.id.to_string(),
            "paged_user_id": user_id,
            "channel": channel,
            "page_attempt_id": attempt.id.to_string(),
            "severity": incident.severity.as_str(),
        }),
    );
    attempt
}


/// Resolve the escalation target of `step` for `incident` to a
/// concrete user id: the next rotation member, the step's manager,
/// or the step's user.
pub(crate) fn escalation_target_user(
    state: &IncidentsState,
    incident: &Incident,
    step: &EscalationStep,
) -> Option<String> {
    match step.target.as_str() {
        "manager" => step.manager_id.clone(),
        "user" => step.user_id.clone(),
        _ => {
            // "responder": the next member of the incident's
            // rotation after the current responder, else the
            // current on-call member, else the first responder.
            if let Some(rotation_id) = incident.rotation_id {
                if let Some(rotation) = state
                    .rotations
                    .get(&rotation_id)
                    .map(|entry| entry.value().clone())
                {
                    if let Some(current) = &incident.current_responder {
                        if let Some(position) =
                            rotation.members.iter().position(|member| member == current)
                        {
                            if !rotation.members.is_empty() {
                                let next = (position + 1) % rotation.members.len();
                                return Some(rotation.members[next].clone());
                            }
                        }
                    }
                    return on_call_member(state, &rotation, Utc::now());
                }
            }
            incident.responders.first().cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_handover_time_accepts_24h_clock_times() {
        assert!(valid_handover_time("00:00"));
        assert!(valid_handover_time("09:30"));
        assert!(valid_handover_time("23:59"));
        assert!(!valid_handover_time("24:00"));
        assert!(!valid_handover_time("09:60"));
        assert!(!valid_handover_time("9:30"));
        assert!(!valid_handover_time("09:3"));
        assert!(!valid_handover_time("ab:cd"));
        assert!(!valid_handover_time("09:30:00"));
    }

    #[test]
    fn valid_timezone_accepts_iana_names_and_utc() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Asia/Jakarta"));
        assert!(valid_timezone("America/New_York"));
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("NoSlash"));
        assert!(!valid_timezone("Bad//Zone"));
        assert!(!valid_timezone("Bad Zone/Here"));
    }

    #[test]
    fn validate_user_ids_trims_and_dedupes() {
        let ids = validate_user_ids(
            &[" user-1 ".to_string(), "user-1".to_string(), "user-2".to_string()],
            "members",
            10,
        )
        .expect("valid ids");
        assert_eq!(ids, vec!["user-1".to_string(), "user-2".to_string()]);
        assert!(validate_user_ids(&["  ".to_string()], "members", 10).is_err());
        assert!(validate_user_ids(&["a".to_string(), "b".to_string()], "members", 1).is_err());
    }

    fn step(after_minutes: i64, target: &str) -> EscalationStep {
        EscalationStep {
            after_minutes,
            target: target.to_string(),
            user_id: None,
            manager_id: None,
        }
    }

    #[test]
    fn validate_escalation_steps_accepts_increasing_responder_steps() {
        let steps = vec![step(0, "responder"), step(15, "responder"), step(60, "responder")];
        assert!(validate_escalation_steps(&steps).is_ok());
    }

    #[test]
    fn validate_escalation_steps_rejects_bad_shapes() {
        assert!(validate_escalation_steps(&[]).is_err());
        assert!(validate_escalation_steps(&[step(15, "responder"), step(15, "responder")])
            .is_err());
        assert!(validate_escalation_steps(&[step(10, "responder"), step(5, "responder")])
            .is_err());
        assert!(validate_escalation_steps(&[step(-1, "responder")]).is_err());
        assert!(validate_escalation_steps(&[step(0, "manager")]).is_err());
        let mut manager = step(0, "manager");
        manager.manager_id = Some("mgr-1".to_string());
        assert!(validate_escalation_steps(&[manager]).is_ok());
        assert!(validate_escalation_steps(&[step(0, "pager")]).is_err());
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
