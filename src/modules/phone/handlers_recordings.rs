//! HTTP handlers for call recordings: start, stop, list,
//! and attach (storage reference only).
//!
//! Recording is consent-gated: a recording may only start
//! when the call carries `recording_consent` (or the tenant
//! settings have explicitly relaxed the gate, in which case
//! the row still records `consent_given: false` so the gap
//! stays visible). Recording bytes are never stored here:
//! the recording row holds only metadata plus the storage
//! layer reference (`storage_target` / `storage_key`, plus an
//! optional signed URL) once the client attaches the stored
//! object. This mirrors the meetings module's recording
//! bookkeeping.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_recording, load_visible_call, ok, paginate, require_text,
    resolve_tenant,
};
use super::state::PhoneState;
use super::types::{
    AttachRecordingRequest, CallRecording, RecordingStatus, EVENT_RECORDING_STARTED,
    EVENT_RECORDING_STOPPED, MAX_LABEL_LEN,
};

/// Path carrying a call id.
#[derive(Debug, Deserialize)]
struct CallPath {
    call_id: String,
}

/// Path carrying a call id and a recording id.
#[derive(Debug, Deserialize)]
struct RecordingPath {
    call_id: String,
    recording_id: String,
}

pub(crate) async fn list_recordings(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let status_filter = params.filters.get("status").cloned();
        let mut items = state.call_recordings(call.id);
        if let Some(status) = &status_filter {
            items.retain(|recording| recording.status.as_str() == status);
        }
        let views: Vec<Value> = items
            .iter()
            .map(|recording| serde_json::to_value(recording).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn start_recording(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !matches!(
            call.status,
            super::types::CallStatus::Active | super::types::CallStatus::Held
        ) {
            return Err(UnifiedError::conflict(format!(
                "Only a live call can be recorded (now {})",
                call.status.as_str()
            )));
        }
        let settings = state.current_settings(&tenant_id);
        if settings.recording_requires_consent && !call.recording_consent {
            return Err(UnifiedError::bad_request(
                "Recording requires explicit consent on this call (recording_consent is false)",
            ));
        }
        if state
            .call_recordings(call.id)
            .iter()
            .any(|recording| recording.status == RecordingStatus::Recording)
        {
            return Err(UnifiedError::conflict(
                "This call already has a recording in progress",
            ));
        }
        let now = Utc::now();
        let recording = CallRecording {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            call_id: call.id,
            started_by: caller.actor.clone(),
            status: RecordingStatus::Recording,
            consent_given: call.recording_consent,
            consent_by: call.recording_consent_by.clone(),
            storage_target: None,
            storage_key: None,
            url: None,
            size_bytes: None,
            duration_secs: None,
            started_at: now,
            ended_at: None,
            created_at: now,
            updated_at: now,
        };
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            EVENT_RECORDING_STARTED,
            json!({
                "tenant_id": tenant_id,
                "call_id": call.id.to_string(),
                "recording_id": recording.id.to_string(),
                "started_by": caller.actor,
                "consent_given": recording.consent_given,
            }),
        );
        Ok(json!(recording))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn stop_recording(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RecordingPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let mut recording = load_recording(&state, &tenant_id, &path.recording_id)?;
        if recording.call_id != call.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {}",
                recording.id
            )));
        }
        if recording.status != RecordingStatus::Recording {
            return Err(UnifiedError::conflict(format!(
                "A {} recording cannot be stopped",
                recording.status.as_str()
            )));
        }
        let now = Utc::now();
        recording.status = RecordingStatus::Stopped;
        recording.ended_at = Some(now);
        recording.duration_secs = Some((now - recording.started_at).num_seconds().max(0));
        recording.updated_at = now;
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            EVENT_RECORDING_STOPPED,
            json!({
                "tenant_id": tenant_id,
                "call_id": call.id.to_string(),
                "recording_id": recording.id.to_string(),
                "actor_id": caller.actor,
                "duration_secs": recording.duration_secs,
            }),
        );
        Ok(json!(recording))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Attach a stored recording object (storage target + key) to
/// a stopped recording. The bytes live in the storage layer;
/// this row only keeps the reference.
pub(crate) async fn attach_recording(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RecordingPath>,
    body: web::Json<AttachRecordingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let mut recording = load_recording(&state, &tenant_id, &path.recording_id)?;
        if recording.call_id != call.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {}",
                recording.id
            )));
        }
        if recording.status != RecordingStatus::Stopped {
            return Err(UnifiedError::conflict(
                "Only a stopped recording can be attached to a stored object",
            ));
        }
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_LABEL_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", 1000)?;
        if let Some(size) = input.size_bytes {
            if size < 0 {
                return Err(UnifiedError::bad_request("size_bytes must not be negative"));
            }
            recording.size_bytes = Some(size);
        }
        if let Some(duration) = input.duration_secs {
            if duration < 0 {
                return Err(UnifiedError::bad_request(
                    "duration_secs must not be negative",
                ));
            }
            recording.duration_secs = Some(duration);
        }
        recording.storage_target = Some(storage_target);
        recording.storage_key = Some(storage_key);
        recording.url = match &input.url {
            Some(raw) => Some(require_text(raw, "url", 2000)?),
            None => None,
        };
        recording.status = RecordingStatus::Stored;
        recording.updated_at = Utc::now();
        state.recordings.insert(recording.id, recording.clone());
        Ok(json!(recording))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Stop every in-progress recording of a call (used when the
/// call ends while a recording is still running).
pub(crate) fn stop_open_recordings(state: &PhoneState, call_id: Uuid) {
    let now = Utc::now();
    for mut entry in state.recordings.iter_mut() {
        if entry.value().call_id == call_id && entry.value().status == RecordingStatus::Recording {
            entry.value_mut().status = RecordingStatus::Stopped;
            entry.value_mut().ended_at = Some(now);
            entry.value_mut().duration_secs =
                Some((now - entry.value().started_at).num_seconds().max(0));
            entry.value_mut().updated_at = now;
        }
    }
}
