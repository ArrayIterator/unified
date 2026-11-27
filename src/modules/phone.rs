// Phone module - WebRTC voice and video calling over the
// web (internet-only): 1:1 and group calls, store-and-forward
// signalling, call queues and routing rules as data,
// presence, IVR menus and auto-response scripts as data,
// consent-gated recording references, and AI call summaries.
//
// Routes live under `/api/v1/phone`:
//
//   Calls:
//     GET/POST /calls - list calls / initiate a call.
//     GET/PATCH/DELETE /calls/{call_id}.
//     POST /calls/{call_id}/accept, /reject, /end, /hold,
//       /resume, /invite.
//     GET  /calls/{call_id}/participants.
//     GET  /calls/{call_id}/summary - the stored AI summary.
//     POST /calls/{call_id}/summarize - run the AI summary.
//     GET  /call-logs - ended and missed calls.
//   Signalling (WebRTC, media stays peer-to-peer):
//     POST /calls/{call_id}/signal - offer / answer / ICE.
//     GET  /calls/{call_id}/signals - the signals inbox.
//     GET  /ws?call={id} - WebSocket feed of one call.
//   Queues and routing:
//     GET/POST /queues, GET/PATCH/DELETE /queues/{queue_id}.
//     GET/POST /queues/{queue_id}/members, DELETE
//       /queues/{queue_id}/members/{member_id}.
//     POST /queues/{queue_id}/join - wait in a queue.
//     GET  /queues/{queue_id}/waiting - who is waiting.
//     POST /queues/{queue_id}/answer - admin answers next.
//     GET  /admin/queue - admin queue dashboard.
//     GET/POST /routing-rules, GET/PATCH/DELETE
//       /routing-rules/{rule_id}.
//     POST /routing-rules/evaluate - first matching rule.
//   Presence:
//     GET  /presence, GET /presence/{user_id}.
//     POST /presence - set your own status.
//     POST /presence/heartbeat - refresh last-seen.
//   IVR and auto-response:
//     GET/POST /ivr-menus, GET/PATCH/DELETE
//       /ivr-menus/{menu_id}.
//     GET/POST /ivr-menus/{menu_id}/nodes, DELETE
//       /ivr-menus/{menu_id}/nodes/{node_id}.
//     POST /ivr-menus/{menu_id}/resolve - one key press.
//     GET/POST /auto-response-scripts, GET/PATCH/DELETE
//       /auto-response-scripts/{script_id}.
//     POST /auto-response - answer a routine caller question.
//   Recordings (metadata only - references, never bytes):
//     GET  /calls/{call_id}/recordings.
//     POST /calls/{call_id}/recordings/start.
//     POST /calls/{call_id}/recordings/{recording_id}/stop,
//       /attach.
//   AI and settings:
//     POST /ai/{capability} - run a registered phone AI
//       capability through the AI module's gated runner.
//     GET  /settings, PATCH /settings.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory call, participant, signal, queue, member, waiter,
// routing-rule, presence, IVR-menu, IVR-node,
// auto-response-script, recording, and settings management;
// the call status machine (ringing -> active -> held ->
// active, ringing/active/held -> ended, ringing -> missed,
// enforced by a single pure transition function); call logs
// with caller/callee, started/ended times, duration, and
// outcome; WebRTC *signalling* (offer / answer / ICE
// exchange stored and relayed through the existing WebSocket
// hub, with an inbox for reconnecting clients); call queues
// as data with sequential / simultaneous / round-robin ring
// order math, ring timeouts, max wait, and overflow targets;
// routing rules as data (caller pattern, time window,
// priority, target queue/user/IVR menu); presence per user
// with stale entries reading as offline; IVR key-press trees
// as data with pure key resolution; auto-response scripts as
// data (template match first, AI capability second); the AI
// capabilities `answer_caller_question`, `summarize_call`,
// and `route_caller`, run through the AI module's gated
// runner with grounding from the stored menus, scripts, and
// call facts; consent-gated recording bookkeeping whose
// bytes live in the storage layer; and tenant scoping on
// every row with cross-tenant access reading as 404, never
// 403. What is NOT built here: durable storage until the
// migration runs (the DashMap stores are authoritative;
// migration `2026-11-22-000116_phone` is the persistence
// target); the actual media transport is browser-to-browser
// WebRTC - there is NO media server, NO SFU, and NO TURN
// relay in this round, so the server NEVER relays media, and
// a group call depends on every participant's browser keeping
// a peer connection to every other participant; queue wait
// expiry is recorded (join time + `max_wait_secs` + overflow
// target) but no background expiry pass runs yet, so an
// expired waiter is visible on the dashboard until an admin
// or a future scheduler pass clears it; the AI provider
// itself (the ai module owns providers, gating, and usage -
// this module only declares capabilities and calls the
// runner); and transcription (the summarize endpoint accepts
// a caller-supplied transcript or transcript reference -
// there is no transcription pipeline in this module yet).
//
// REUSED FROM MEETINGS vs BUILT FRESH. Reused, same shapes
// and same contracts so clients need no new vocabulary:
// the signalling message vocabulary (`offer`, `answer`,
// `ice_candidate`, one ICE candidate per message, payload a
// JSON object carrying SDP or ICE); the store-and-broadcast
// relay (`relay_signal` validation, then
// `global_hub().broadcast` on the shared WebSocket hub, with
// receivers filtering on the call/room id plus
// `recipient_id`); the WebSocket session shape (subscribe,
// forward only the call's events addressed to the caller or
// broadcast, never echo the caller's own messages, `ping` /
// `pong`, inbound `{"type": "signal", ...}` frames through
// the same relay validation as REST); the recording
// bookkeeping shape (metadata only, storage target + key
// attached after stop); and the presence heartbeat idea
// (stale entries hidden). Built fresh for calls, because
// meetings are scheduled rooms and calls are not: the call
// status machine (ringing/active/held/ended/missed) and the
// call log with direction and outcome; the store-and-forward
// signals inbox (meetings has no inbox - a call client that
// reconnects must not lose an offer); queues with ring-order
// math, waiters, and admin answering; routing rules with
// caller patterns and time windows; per-user presence owned
// by this module (meetings presence is per-room); IVR menus
// and auto-response scripts as data; the consent gate on
// recording; and the AI summary stored on the call log.
//
// CLIENT CONTRACT (desktop app and Android clients reuse
// this same API later - no second API is planned):
//   Auth: every endpoint is authenticated
//     (`RouteAuth::Authenticated`); the caller is the
//     server-verified caller from
//     `unified_core::caller::require_caller` (user id plus
//     server-resolved role). There is no anonymous call
//     initiation. Clients send the same auth the web client
//     sends (session / bearer token per the auth module);
//     the `X-User-Id` header pattern the meetings module
//     documents is NOT used here.
//   Tenant: every request resolves its tenant from the
//     middleware's resolved tenant context; there is no
//     tenant id in request bodies.
//   Calls: POST /calls with `callee_id` (or `callee_ids`
//     for a group call), `kind` (`voice` | `video`),
//     optional `subject`, `recording_consent`,
//     `queue_code`, `ivr_menu_code`. The response is the
//     call with its participants. The callee accepts with
//     POST /calls/{id}/accept (ringing -> active, the call
//     starts), rejects with /reject, ends with /end.
//   Signalling: POST /calls/{id}/signal with
//     `{"kind": "offer" | "answer" | "ice_candidate",
//     "recipient_id"?: "<user id>", "payload": {...}}`.
//     `recipient_id` absent means broadcast to the whole
//     call. The server stores the message, broadcasts
//     `{"event": "phone.signal", "call_id", "sender_id",
//     "recipient_id", "kind", "payload", "sent_at"}` on the
//     shared hub, and emits `phone.signal`. A client that
//     reconnects fetches missed messages from
//     GET /calls/{id}/signals (reading marks them
//     delivered). The WebSocket feed is
//     GET /api/v1/phone/ws?call={id}: it forwards only this
//     call's events addressed to the caller or broadcast,
//     and accepts inbound `{"type": "signal", ...}` and
//     `{"type": "ping"}` frames.
//   Presence: POST /presence with `{"status":
//     "available" | "busy" | "do_not_disturb" | "offline",
//     "note"?}`; POST /presence/heartbeat at least every 90
//     seconds while connected; GET /presence/{user_id}
//     reads one user (no row reads as `offline`).
//   Errors: the standard `{"data"}` / `{"error"}`
//     envelope via `unified_core::json`; cross-tenant
//     access reads as 404, never 403.
//   Media: peer-to-peer WebRTC only. Clients exchange SDP
//     and ICE through the signalling endpoints above and
//     connect directly; there is no server media endpoint
//     to call.
//
// Actor identity: the caller is the server-verified caller
// from `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the forums and kyc modules. Capability checks
// use the `role.check` hook and fail closed (no answer means
// not allowed). All handlers take
// `web::Data<Arc<AppContext>>`, because the server registers
// `Arc<AppContext>` (see `src/http/server.rs`:
// `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `phone.call.initiated`, `phone.call.answered`,
//     `phone.call.ended`, `phone.call.missed`,
//     `phone.call.held`, `phone.call.resumed`,
//     `phone.call.summarized`
//   - `phone.queue.joined`, `phone.queue.answered`
//   - `phone.recording.started`, `phone.recording.stopped`
//   - `phone.signal` (one per relayed signalling message)
//   - `phone.presence.updated`, `phone.ivr.resolved`
//   - `notification.requested` (incoming calls and call
//     updates, in the notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` - anonymizes the user's calls,
//     participants, signals, queue memberships and waiters,
//     and recordings to `deleted-user`, and deletes their
//     presence row (presence is live state, not history).
//   - `phone.ai.answer_caller_question_requested` -
//     grounds the answer capability with the tenant's active
//     IVR menus and auto-response scripts.
//   - `phone.ai.summarize_call_requested` - grounds the
//     summary capability with the call's stored facts.
// No presence events exist to listen to: no
// `user.presence.*` or `presence.changed` event is emitted
// anywhere in the tree (verified by grep over
// `src/modules/`), and the chat module's presence is
// per-room state, not a hook event - so presence is owned by
// this module and nothing subscribes to an external feed.

pub mod ai_capabilities;
mod handlers_ai;
mod handlers_calls;
mod handlers_ivr;
mod handlers_presence;
mod handlers_queues;
mod handlers_recordings;
mod handlers_signalling;
mod helpers;
mod hooks;
mod module;
mod requests;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::register;
pub use self::module::PhoneModule;
pub use self::state::PhoneState;
pub use self::types::AddQueueMemberRequest;
pub use self::types::AttachRecordingRequest;
pub use self::types::AutoResponseRequest;
pub use self::types::AutoResponseScript;
pub use self::types::CallKind;
pub use self::types::CallOutcome;
pub use self::types::CallParticipant;
pub use self::types::CallRecording;
pub use self::types::CallStatus;
pub use self::types::CreateIvrMenuRequest;
pub use self::types::CreateIvrNodeRequest;
pub use self::types::CreateQueueRequest;
pub use self::types::CreateRoutingRuleRequest;
pub use self::types::CreateScriptRequest;
pub use self::types::EndCallRequest;
pub use self::types::EvaluateRoutingRequest;
pub use self::types::InitiateCallRequest;
pub use self::types::InviteParticipantRequest;
pub use self::types::IvrAction;
pub use self::types::IvrMenu;
pub use self::types::IvrNode;
pub use self::types::ParticipantStatus;
pub use self::types::PhoneCall;
pub use self::types::PhoneQueue;
pub use self::types::PhoneSettings;
pub use self::types::PresenceEntry;
pub use self::types::PresenceStatus;
pub use self::types::QueueMember;
pub use self::types::QueueWaiter;
pub use self::types::RecordingStatus;
pub use self::types::ResolveIvrRequest;
pub use self::types::RingStrategy;
pub use self::types::RoutingRule;
pub use self::types::SetPresenceRequest;
pub use self::types::SignalKind;
pub use self::types::SignalMessage;
pub use self::types::SignalRequest;
pub use self::types::UpdateCallRequest;
pub use self::types::UpdateIvrMenuRequest;
pub use self::types::UpdateQueueRequest;
pub use self::types::UpdateRoutingRuleRequest;
pub use self::types::UpdateScriptRequest;
pub use self::types::UpdateSettingsRequest;
