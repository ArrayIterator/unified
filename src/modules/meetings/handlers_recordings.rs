//! HTTP handlers: recordings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageProvider;
use crate::http::websocket::global_hub;
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;

// Recordings

pub(crate) async fn list_recordings(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Recording>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" && !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can list this room's recordings",
            ));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Recording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|recording| recording.room_id == room.id)
            .filter(|recording| {
                status_filter
                    .as_ref()
                    .is_none_or(|status| &recording.status == status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn start_recording(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host or a co-host can start a recording",
            ));
        }
        if room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A recording can only start in a live room",
            ));
        }
        let already_recording = state.recordings.iter().any(|entry| {
            entry.value().room_id == room.id && entry.value().status == "recording"
        });
        if already_recording {
            return Err(UnifiedError::conflict(
                "This room already has a recording in progress",
            ));
        }
        let now = Utc::now();
        let id = Uuid::now_v7();
        let recording = Recording {
            id,
            room_id: room.id,
            started_by: actor.clone(),
            status: "recording".to_string(),
            storage_target: None,
            // The default object key the recording client uploads to
            // through the storage module; the attach call confirms
            // the real target and key afterwards.
            storage_key: Some(format!("meetings/{}/{id}.webm", room.id)),
            storage_url: None,
            size_bytes: None,
            duration_secs: None,
            started_at: now,
            stopped_at: None,
            created_at: now,
            updated_at: now,
        };
        state.recordings.insert(id, recording.clone());
        state.publish_room_event(
            "meeting.recording.started",
            room.id,
            json!({
                "recording_id": id.to_string(),
                "started_by": actor,
                "storage_key": recording.storage_key,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => created(recording),
        Err(error) => err(error),
    }
}

pub(crate) async fn stop_recording(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, raw_recording) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let recording_id = parse_uuid(&raw_recording, "recording")?;
        let mut recording = state
            .recordings
            .get(&recording_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Recording not found: {recording_id}")))?;
        if recording.room_id != room.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {recording_id}"
            )));
        }
        let participant = load_participant(&state, room.id, &actor)?;
        // The host, a co-host, or the user who started the recording
        // may stop it.
        let started_by_actor = recording.started_by == actor;
        if !started_by_actor && !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host, a co-host, or the user who started the recording can stop it",
            ));
        }
        if recording.status != "recording" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a recording in progress can be stopped (current status: {})",
                recording.status
            )));
        }
        let now = Utc::now();
        recording.status = "stopped".to_string();
        recording.stopped_at = Some(now);
        recording.updated_at = now;
        state.recordings.insert(recording.id, recording.clone());
        state.publish_room_event(
            "meeting.recording.stopped",
            room.id,
            json!({
                "recording_id": recording.id.to_string(),
                "stopped_by": actor,
                "stopped_at": now.to_rfc3339(),
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

pub(crate) async fn attach_recording(
    state: web::Data<MeetingsState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<AttachRecordingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| async {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, raw_recording) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let participant = load_participant(&state, room.id, &actor)?;
        if !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host or a co-host can attach a recording",
            ));
        }
        let recording_id = parse_uuid(&raw_recording, "recording")?;
        let mut recording = state
            .recordings
            .get(&recording_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Recording not found: {recording_id}"))
            })?;
        if recording.room_id != room.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {recording_id}"
            )));
        }
        if recording.status != "stopped" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a stopped recording can be attached to a stored object (current status: {})",
                recording.status
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", 128)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_KEY_LEN_STORAGE)?;
        // The URL is a reference, not a second copy of the bytes: use
        // the caller's URL when given, otherwise mint a signed URL
        // from the existing storage registry for that target.
        let storage_url = match input.url {
            Some(raw) => Some(require_text(&raw, "url", 2048)?),
            None => {
                let provider = ctx
                    .storage_registry
                    .get(&storage_target)
                    .ok_or_else(|| {
                        UnifiedError::not_found(format!(
                            "Storage target \"{storage_target}\" not found"
                        ))
                    })?;
                Some(
                    provider
                        .signed_url(&storage_key, RECORDING_SIGNED_URL_SECS)
                        .await?,
                )
            }
        };
        recording.status = "stored".to_string();
        recording.storage_target = Some(storage_target);
        recording.storage_key = Some(storage_key);
        recording.storage_url = storage_url;
        recording.size_bytes = input.size_bytes;
        recording.duration_secs = input.duration_secs;
        recording.updated_at = Utc::now();
        state.recordings.insert(recording.id, recording.clone());
        state.publish_room_event(
            "meeting.recording.stored",
            room.id,
            json!({
                "recording_id": recording.id.to_string(),
                "storage_target": recording.storage_target,
                "storage_key": recording.storage_key,
                "storage_url": recording.storage_url,
                "size_bytes": recording.size_bytes,
                "duration_secs": recording.duration_secs,
            }),
        );
        Ok(recording)
    })()
    .await;
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

/// Maximum storage key length for a recording attach, matching the
/// storage module's key limit.
const MAX_KEY_LEN_STORAGE: usize = 1024;
