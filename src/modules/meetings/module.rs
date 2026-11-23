//! Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageProvider;
use crate::http::websocket::global_hub;
use crate::modules::ai;
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
// Meetings module — Zoom-like video meetings over WebRTC.
//
// Routes live under `/api/v1/meetings`:
//
//   Rooms:
//     GET/POST /rooms — list rooms / create a room (scheduled or instant).
//     GET/PATCH/DELETE /rooms/{id}.
//     POST /rooms/{id}/start, /end, /cancel, /regenerate-token.
//     GET  /join/{token} — resolve a join link token to its room.
//     POST /rooms/{id}/join, /leave.
//   People:
//     GET  /rooms/{id}/participants — paginated, filter by status / role.
//     POST /rooms/{id}/participants/{user_id}/promote, /demote, /remove.
//     POST /rooms/{id}/waiting/{user_id}/admit, /deny.
//     GET/POST /rooms/{id}/presence — live presence + heartbeat.
//     POST /rooms/{id}/screen-share — start / stop sharing a screen.
//   In-meeting:
//     GET/POST /rooms/{id}/messages — chat inside the meeting.
//     POST /rooms/{id}/signal — WebRTC signalling relay (offer /
//       answer / ICE candidate) over the shared WebSocket hub.
//     GET  /ws?room={id} — WebSocket feed of this room's events.
//   Recordings:
//     POST /rooms/{id}/recordings/start.
//     POST /rooms/{id}/recordings/{recording_id}/stop, /attach.
//     GET  /rooms/{id}/recordings.
//   History:
//     GET  /rooms/{id}/attendance — who joined, when, for how long.
//     GET  /history — ended and cancelled rooms.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-app room management (create / schedule / join / leave / waiting
// room / roles / presence), WebRTC *signalling* (offer / answer / ICE
// exchange relayed through the existing WebSocket hub), in-meeting
// chat, screen-share state events, and recording bookkeeping whose
// bytes live in the existing storage module. What is NOT built here:
// the actual media transport is browser-to-browser WebRTC — there is
// NO media server and NO SFU (Selective Forwarding Unit) in this
// round, so there is no server-side mixing, transcoding, or recording
// of media streams. A group call therefore depends on every
// participant's browser keeping a peer connection to every other
// participant. Follow-up (not started here): add an SFU / media
// server (for example a Janus / LiveKit / mediasoup gateway) and a
// server-side recording pipeline; until then the recording endpoints
// below only track metadata and reference objects already stored
// through the storage module.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required), exactly
// like the chat module. The paid-room host override is verified
// server-side: the room host, or a caller whose `X-Role` slug holds
// the `meetings.host` permission according to the roles module
// (`role.check` hook). Client-supplied `X-User-Roles` are never
// trusted for entitlement: a header claiming `admin` grants
// nothing on its own.
//
// The join token gates entry: joining by room id requires the
// token from the join link (or host/co-host standing), and the token is
// never serialized with a room except in the host's own create and
// regenerate-token responses, so enumerating rooms cannot harvest
// tokens.
//
// Events emitted on the hook bus:
//   - `meeting.room.created`, `meeting.room.updated`,
//     `meeting.room.deleted`, `meeting.started`, `meeting.ended`,
//     `meeting.cancelled`
//   - `meeting.joined`, `meeting.left`
//   - `meeting.waiting.requested`, `meeting.waiting.admitted`,
//     `meeting.waiting.denied`
//   - `meeting.participant.promoted`, `meeting.participant.demoted`,
//     `meeting.participant.removed`
//   - `meeting.presence`, `meeting.screen_share.started`,
//     `meeting.screen_share.stopped`
//   - `meeting.chat.message`
//   - `meeting.signal` (one per relayed signalling message)
//   - `meeting.recording.started`, `meeting.recording.stopped`,
//     `meeting.recording.stored`
// Events this module listens to:
//   - `scheduler.enqueue` is *emitted* (not listened to) when a room
//     is scheduled: the scheduler module listens to it and queues a
//     job from `{"kind", "payload"}`. See `schedule_meeting`.
//   - `meeting.start_requested` is listened to: that is the event the
//     scheduler would emit when a `meeting.start` job fires, following
//     its own naming rule (`backup.run` -> `backup.run_requested`).
// Events this module emits to other modules:
//   - `product.entitlement.check` with `{"product_id", "user_id"}`,
//     expecting `{"entitled": bool}` in the first hook result. Paid
//     rooms fail closed (503) when no handler answers; see
//     `check_entitlement`.


// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct MeetingsModule {
    state: MeetingsState,
}

impl MeetingsModule {
    pub fn new() -> Self {
        Self {
            state: MeetingsState::default(),
        }
    }
}

impl Default for MeetingsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for MeetingsModule {
    fn id(&self) -> &'static str {
        "meetings"
    }

    fn name(&self) -> &'static str {
        "Meetings"
    }

    fn description(&self) -> &'static str {
        "Video meetings over WebRTC: rooms, waiting room, roles, signalling over the shared WebSocket hub, in-meeting chat, screen-share events, recording references, attendance, and paid rooms via product entitlements. Media transport is browser-to-browser WebRTC; no media server or SFU is built in."
    }

    /// Meetings is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn requires(&self) -> &[&'static str] {
        &["scheduler", "storage", "store"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "scheduler", "storage", "store"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("meetings", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "meeting.start_requested",
            "meetings",
            Arc::new(move |ctx: &mut HookContext| {
                on_meeting_start_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/meetings")
                .app_data(state)
                .route("/rooms", web::get().to(list_rooms))
                .route("/rooms", web::post().to(create_room))
                .route("/rooms/{id}", web::get().to(get_room))
                .route("/rooms/{id}", web::patch().to(update_room))
                .route("/rooms/{id}", web::delete().to(delete_room))
                .route("/rooms/{id}/start", web::post().to(start_room))
                .route("/rooms/{id}/end", web::post().to(end_room))
                .route("/rooms/{id}/cancel", web::post().to(cancel_room))
                .route(
                    "/rooms/{id}/regenerate-token",
                    web::post().to(regenerate_token),
                )
                .route("/join/{token}", web::get().to(join_by_token))
                .route("/rooms/{id}/join", web::post().to(join_room))
                .route("/rooms/{id}/leave", web::post().to(leave_room))
                .route("/rooms/{id}/participants", web::get().to(list_participants))
                .route(
                    "/rooms/{id}/participants/{user_id}/promote",
                    web::post().to(promote_participant),
                )
                .route(
                    "/rooms/{id}/participants/{user_id}/demote",
                    web::post().to(demote_participant),
                )
                .route(
                    "/rooms/{id}/participants/{user_id}/remove",
                    web::post().to(remove_participant),
                )
                .route(
                    "/rooms/{id}/waiting/{user_id}/admit",
                    web::post().to(admit_waiting),
                )
                .route(
                    "/rooms/{id}/waiting/{user_id}/deny",
                    web::post().to(deny_waiting),
                )
                .route("/rooms/{id}/presence", web::get().to(list_presence))
                .route("/rooms/{id}/presence", web::post().to(heartbeat_presence))
                .route("/rooms/{id}/screen-share", web::post().to(set_screen_share))
                .route("/rooms/{id}/messages", web::get().to(list_messages))
                .route("/rooms/{id}/messages", web::post().to(post_message))
                .route("/rooms/{id}/signal", web::post().to(post_signal))
                .route("/rooms/{id}/recordings", web::get().to(list_recordings))
                .route(
                    "/rooms/{id}/recordings/start",
                    web::post().to(start_recording),
                )
                .route(
                    "/rooms/{id}/recordings/{recording_id}/stop",
                    web::post().to(stop_recording),
                )
                .route(
                    "/rooms/{id}/recordings/{recording_id}/attach",
                    web::post().to(attach_recording),
                )
                .route("/rooms/{id}/attendance", web::get().to(list_attendance))
                .route("/history", web::get().to(list_history))
                .route("/ws", web::get().to(meetings_ws)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms", owner: "meetings", description: "List meeting rooms (paginated, filter by status, host_id, access_model, product_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms", owner: "meetings", description: "Create a meeting room (scheduled or instant).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Get one meeting room.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Update a meeting room (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Delete a scheduled, ended, or cancelled room (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/start", owner: "meetings", description: "Start a scheduled room (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/end", owner: "meetings", description: "End a live room (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/cancel", owner: "meetings", description: "Cancel a scheduled room (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/regenerate-token", owner: "meetings", description: "Regenerate a room join token, invalidating old links (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/join/{token}", owner: "meetings", description: "Resolve a join link token to its room.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/join", owner: "meetings", description: "Join a room (waiting room, seat limit, and paid entitlement enforced).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/leave", owner: "meetings", description: "Leave a room (attendance is recorded).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/participants", owner: "meetings", description: "List room participants (paginated, filter by status, role).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/promote", owner: "meetings", description: "Promote a participant to co-host (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/demote", owner: "meetings", description: "Demote a co-host to participant (host only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/remove", owner: "meetings", description: "Remove a participant from a room (host or co-host).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/waiting/{user_id}/admit", owner: "meetings", description: "Admit a waiting-room user into the room (host or co-host).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/waiting/{user_id}/deny", owner: "meetings", description: "Deny a waiting-room user (host or co-host).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/presence", owner: "meetings", description: "Live presence of joined participants (stale entries hidden).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/presence", owner: "meetings", description: "Presence heartbeat with media state (joined participants).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/screen-share", owner: "meetings", description: "Start or stop sharing a screen (event only; media stays peer-to-peer).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/messages", owner: "meetings", description: "List in-meeting chat messages (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/messages", owner: "meetings", description: "Post an in-meeting chat message.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/signal", owner: "meetings", description: "Relay one WebRTC signalling message (offer, answer, ICE candidate) in a room.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/recordings", owner: "meetings", description: "List recordings of a room (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/start", owner: "meetings", description: "Start a recording (metadata only; bytes go through the storage module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/{recording_id}/stop", owner: "meetings", description: "Stop a recording.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/{recording_id}/attach", owner: "meetings", description: "Attach a stored recording object (storage target + key) to a stopped recording.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/attendance", owner: "meetings", description: "Attendance of a room: who joined, when, for how long.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/history", owner: "meetings", description: "Ended and cancelled rooms (paginated, filter by host_id, from, to).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/meetings/ws", owner: "meetings", description: "WebSocket feed of one room's events (?room={id}); also accepts signalling messages.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MeetingsModule::new()));
}
