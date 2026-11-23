// webinars -- Recording and replay handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Recording / replay handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_recordings(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WebinarRecording>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<WebinarRecording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        let _ = actor;
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = a.created_at.cmp(&b.created_at);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn attach_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachRecordingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRecording> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can attach recordings",
            ));
        }
        if !webinar.recording_enabled {
            return Err(UnifiedError::unprocessable(
                "Recording is disabled for this webinar",
            ));
        }
        let meeting_recording_id = match &input.meeting_recording_id {
            Some(raw) => Some(parse_uuid(raw, "meeting_recording")?),
            None => None,
        };
        let title = match &input.title {
            Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
            None => None,
        };
        let status = match &input.status {
            Some(raw) => {
                let cleaned = require_text(raw, "status", 32)?;
                match cleaned.as_str() {
                    "processing" | "ready" | "failed" => cleaned,
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "status must be processing, ready, or failed",
                        ));
                    }
                }
            }
            None => "processing".to_string(),
        };
        let storage_target = require_text(&input.storage_target, "storage_target", 128)?;
        let storage_key = require_text(&input.storage_key, "storage_key", 512)?;
        let storage_url = match &input.storage_url {
            Some(raw) => {
                let cleaned = require_text(raw, "storage_url", 1024)?;
                if !valid_url(&cleaned) {
                    return Err(UnifiedError::bad_request(
                        "storage_url must be an http:// or https:// URL",
                    ));
                }
                Some(cleaned)
            }
            None => None,
        };
        let now = Utc::now();
        let recording = WebinarRecording {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            meeting_recording_id,
            title,
            status: status.clone(),
            storage_target: Some(storage_target),
            storage_key: Some(storage_key),
            storage_url,
            size_bytes: input.size_bytes,
            duration_secs: input.duration_secs,
            created_at: now,
            updated_at: now,
        };
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            "webinar.recording.attached",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "meeting_recording_id": meeting_recording_id.map(|id| id.to_string()),
                "status": status,
                "storage_target": recording.storage_target,
                "storage_key": recording.storage_key,
                "attached_by": actor,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => created(recording),
        Err(error) => err(error),
    }
}

pub(crate) async fn publish_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRecording> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, recording_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can publish recordings",
            ));
        }
        let mut recording = load_recording(&state, webinar.id, &recording_id)?;
        if recording.status == "ready" {
            return Err(UnifiedError::unprocessable(
                "This recording is already ready for replay",
            ));
        }
        recording.status = "ready".to_string();
        recording.updated_at = Utc::now();
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            "webinar.recording.ready",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "published_by": actor,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, recording_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete recordings",
            ));
        }
        let recording = load_recording(&state, webinar.id, &recording_id)?;
        state.recordings.remove(&recording.id);
        state.emit(
            "webinar.recording.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "recording_id": recording.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_replay(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ReplayView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        let staff = is_staff(&webinar, &actor, &roles);
        let registered = find_registration(&state, webinar.id, &actor)
            .is_some_and(|row| row.status == "registered");
        let attended = state
            .attendance
            .iter()
            .any(|entry| entry.value().webinar_id == webinar.id && entry.value().user_id == actor);
        if webinar.status != WebinarStatus::Ended && !staff {
            return Err(UnifiedError::unprocessable(
                "Replay is only available once the webinar has ended",
            ));
        }
        if !staff && !registered && !attended {
            return Err(UnifiedError::forbidden(
                "Replay is only available to the webinar staff and its registered attendees",
            ));
        }
        let mut recordings: Vec<WebinarRecording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "ready")
            .collect();
        recordings.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(ReplayView {
            webinar: webinar.clone(),
            recordings,
            can_watch: true,
            attended,
        })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}
