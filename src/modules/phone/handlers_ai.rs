//! HTTP handlers for the phone AI surface and per-tenant
//! settings.
//!
//! The AI surface mirrors the kyc module's `ai_capability`
//! handler: one registered capability at a time through the
//! AI module's gated runner (`run_ai_capability`, capability
//! allowlist gate first). The post-call summary runs the
//! `summarize_call` capability over the call's stored facts
//! (participants, duration, outcome, and an optional caller
//! supplied transcript) and stores the parsed summary on the
//! call log. The AI never books, charges, or changes a call
//! on its own.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

use super::helpers::{
    call_view, caller, err, load_visible_call, ok, require_permission, resolve_tenant,
};
use super::state::PhoneState;
use super::types::{
    parse_ai_summary, UpdateSettingsRequest, EVENT_CALL_SUMMARIZED, PERM_PHONE_MANAGE,
};
use crate::modules::ai;

/// Path carrying a call id.
#[derive(Debug, Deserialize)]
struct CallPath {
    call_id: String,
}

/// Body for the summarize endpoint: an optional transcript
/// (caller-supplied text or a transcription-hook reference)
/// the AI summarizes. Without a transcript the summary is
/// built from the call's stored facts alone.
#[derive(Debug, Deserialize)]
pub struct SummarizeCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_ref: Option<String>,
}

/// Run one registered phone AI capability through the AI
/// module's gated runner, exactly like the kyc AI endpoint.
pub(crate) async fn ai_capability(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let _tenant_id = match resolve_tenant(&req) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) =
        super::helpers::require_permission(&ctx, &caller.role, super::types::PERM_PHONE_VIEW)
    {
        return err(error, debug);
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(&ctx, &ai_caller, company_id, "phone", &capability, input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Summarize an ended call with the `summarize_call`
/// capability and store the summary on the call log. Only
/// ended or missed calls can be summarized; re-running
/// replaces the stored summary.
pub(crate) async fn summarize_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    body: web::Json<SummarizeCallRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(PhoneCallFacts, String, Option<String>)> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !call.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Only an ended or missed call can be summarized (now {})",
                call.status.as_str()
            )));
        }
        let transcript = match &input.transcript {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.len() > 50000 {
                    return Err(UnifiedError::bad_request(
                        "transcript must be at most 50000 characters",
                    ));
                }
                Some(trimmed.to_string())
            }
            None => None,
        };
        let transcript_ref = match &input.transcript_ref {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            None => None,
        };
        let facts = PhoneCallFacts {
            tenant_id,
            call_id: call.id,
            caller_id: call.caller_id.clone(),
            kind: call.kind.as_str().to_string(),
            status: call.status.as_str().to_string(),
            outcome: call.outcome.map(|outcome| outcome.as_str().to_string()),
            duration_secs: call.duration_secs,
            subject: call.subject.clone(),
            participant_ids: state
                .call_participants(call.id)
                .iter()
                .map(|participant| participant.user_id.clone())
                .collect(),
            started_at: call.started_at.map(|time| time.to_rfc3339()),
            ended_at: call.ended_at.map(|time| time.to_rfc3339()),
        };
        Ok((facts, caller.actor, transcript.or(transcript_ref)))
    })();
    let (facts, actor, transcript) = match result {
        Ok(values) => values,
        Err(error) => return err(error, debug),
    };
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    let input = json!({
        "tenant_id": facts.tenant_id,
        "call_id": facts.call_id.to_string(),
        "caller_id": facts.caller_id,
        "kind": facts.kind,
        "status": facts.status,
        "outcome": facts.outcome,
        "duration_secs": facts.duration_secs,
        "subject": facts.subject,
        "participant_ids": facts.participant_ids,
        "started_at": facts.started_at,
        "ended_at": facts.ended_at,
        "transcript": transcript,
    });
    let output = match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "phone",
        "summarize_call",
        input,
    )
    .await
    {
        Ok(output) => output,
        Err(error) => return err(error, debug),
    };
    let output_text = output
        .get("output")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let model = output
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let summary = match parse_ai_summary(&output_text) {
        Some(summary) => summary,
        None => {
            return err(
                UnifiedError::internal("AI call summary output could not be parsed as a summary"),
                debug,
            );
        }
    };
    let mut call = match state
        .calls
        .get(&facts.call_id)
        .map(|entry| entry.value().clone())
    {
        Some(call) => call,
        None => {
            return err(
                UnifiedError::not_found(format!("Call not found: {}", facts.call_id)),
                debug,
            );
        }
    };
    call.summary = Some(summary.clone());
    call.summary_model = model.clone();
    call.updated_at = Utc::now();
    state.calls.insert(call.id, call.clone());
    state.emit(
        EVENT_CALL_SUMMARIZED,
        json!({
            "tenant_id": facts.tenant_id,
            "call_id": call.id.to_string(),
            "actor_id": actor,
            "summary_model": model,
        }),
    );
    ok(call_view(&state, &call))
}

/// The call facts handed to the summary capability.
struct PhoneCallFacts {
    tenant_id: String,
    call_id: Uuid,
    caller_id: String,
    kind: String,
    status: String,
    outcome: Option<String>,
    duration_secs: Option<i64>,
    subject: Option<String>,
    participant_ids: Vec<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
}

// Per-tenant settings (manager for writes)

pub(crate) async fn get_settings(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let settings = state.current_settings(&tenant_id);
        Ok(json!(settings))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.current_settings(&tenant_id);
        if let Some(timeout) = input.default_ring_timeout_secs {
            if !(1..=600).contains(&timeout) {
                return Err(UnifiedError::bad_request(
                    "default_ring_timeout_secs must be between 1 and 600",
                ));
            }
            settings.default_ring_timeout_secs = timeout;
        }
        if let Some(max_duration) = input.max_call_duration_secs {
            if max_duration < 0 || max_duration > 86400 {
                return Err(UnifiedError::bad_request(
                    "max_call_duration_secs must be between 0 and 86400",
                ));
            }
            settings.max_call_duration_secs = max_duration;
        }
        if let Some(requires_consent) = input.recording_requires_consent {
            settings.recording_requires_consent = requires_consent;
        }
        if let Some(timeout) = input.missed_call_timeout_secs {
            if !(1..=600).contains(&timeout) {
                return Err(UnifiedError::bad_request(
                    "missed_call_timeout_secs must be between 1 and 600",
                ));
            }
            settings.missed_call_timeout_secs = timeout;
        }
        if let Some(allow_group) = input.allow_group_calls {
            settings.allow_group_calls = allow_group;
        }
        if let Some(max_group) = input.max_group_participants {
            if !(2..=50).contains(&max_group) {
                return Err(UnifiedError::bad_request(
                    "max_group_participants must be between 2 and 50",
                ));
            }
            settings.max_group_participants = max_group;
        }
        if let Some(queue_code) = &input.default_queue_code {
            settings.default_queue_code = match queue_code {
                Some(code) => {
                    super::helpers::load_queue_by_code(&state, &tenant_id, code)?;
                    Some(code.clone())
                }
                None => None,
            };
        }
        if let Some(menu_code) = &input.default_ivr_menu_code {
            settings.default_ivr_menu_code = match menu_code {
                Some(code) => {
                    super::helpers::load_menu_by_code(&state, &tenant_id, code)?;
                    Some(code.clone())
                }
                None => None,
            };
        }
        state.save_settings(&tenant_id, settings.clone());
        Ok(json!(settings))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
