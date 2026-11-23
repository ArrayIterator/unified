//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::{ai_capability, get_settings, summarize_call, update_settings};
use super::handlers_calls::{
    accept_call, delete_call, end_call, get_call, get_call_summary, hold_call, initiate_call,
    invite_participant, list_call_logs, list_calls, list_participants, reject_call, resume_call,
    update_call,
};
use super::handlers_ivr::{
    add_ivr_node, auto_response, create_ivr_menu, create_script, delete_ivr_menu, delete_ivr_node,
    delete_script, get_ivr_menu, get_script, list_ivr_menus, list_ivr_nodes, list_scripts,
    resolve_ivr, update_ivr_menu, update_script,
};
use super::handlers_presence::{get_presence, heartbeat_presence, list_presence, set_presence};
use super::handlers_queues::{
    add_queue_member, admin_queue_dashboard, answer_queue, create_queue, create_routing_rule,
    delete_queue, delete_routing_rule, evaluate_routing, get_queue, get_routing_rule, join_queue,
    list_queue_members, list_queue_waiting, list_queues, list_routing_rules, remove_queue_member,
    update_queue, update_routing_rule,
};
use super::handlers_recordings::{
    attach_recording, list_recordings, start_recording, stop_recording,
};
use super::handlers_signalling::{list_signals, phone_ws, post_signal};
use super::hooks::{
    ai_answer_event, ai_summarize_event, on_ai_answer_requested, on_ai_summarize_requested,
    on_user_deleted,
};
use super::state::PhoneState;
use super::types::EVENT_USER_DELETED;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PhoneModule {
    pub(crate) state: PhoneState,
}

impl PhoneModule {
    pub fn new() -> Self {
        Self {
            state: PhoneState::default(),
        }
    }
}

impl Default for PhoneModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PhoneModule {
    fn id(&self) -> &'static str {
        "phone"
    }

    fn name(&self) -> &'static str {
        "Phone"
    }

    fn description(&self) -> &'static str {
        "WebRTC voice and video calling over the web (internet-only): 1:1 and group calls with ringing, active, held, ended and missed states, store-and-forward signalling (offer, answer, ICE candidates) over REST and the shared WebSocket hub with media staying peer-to-peer, call queues and routing rules as data, presence per user, IVR menus and auto-response scripts as data with AI answering of routine caller questions, consent-gated recording references, AI call summaries on the call log, and a client contract desktop and Android clients can reuse."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "ai"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "ai"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("phone", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "phone",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        // The AI module's runner emits this grounding event for
        // the answer capability; this module answers with the
        // tenant's active IVR menus and auto-response scripts.
        let state = self.state.clone();
        bus.register(
            ai_answer_event(),
            "phone",
            Arc::new(move |ctx: &mut HookContext| on_ai_answer_requested(state.clone(), ctx)),
        );
        // The AI module's runner emits this grounding event for
        // the summary capability; this module answers with the
        // call's stored facts.
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "phone",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/phone")
                .app_data(state)
                .route("/calls", web::get().to(list_calls))
                .route("/calls", web::post().to(initiate_call))
                .route("/calls/{call_id}", web::get().to(get_call))
                .route("/calls/{call_id}", web::patch().to(update_call))
                .route("/calls/{call_id}", web::delete().to(delete_call))
                .route("/calls/{call_id}/accept", web::post().to(accept_call))
                .route("/calls/{call_id}/reject", web::post().to(reject_call))
                .route("/calls/{call_id}/end", web::post().to(end_call))
                .route("/calls/{call_id}/hold", web::post().to(hold_call))
                .route("/calls/{call_id}/resume", web::post().to(resume_call))
                .route(
                    "/calls/{call_id}/invite",
                    web::post().to(invite_participant),
                )
                .route(
                    "/calls/{call_id}/participants",
                    web::get().to(list_participants),
                )
                .route("/calls/{call_id}/signal", web::post().to(post_signal))
                .route("/calls/{call_id}/signals", web::get().to(list_signals))
                .route("/calls/{call_id}/summary", web::get().to(get_call_summary))
                .route("/calls/{call_id}/summarize", web::post().to(summarize_call))
                .route(
                    "/calls/{call_id}/recordings",
                    web::get().to(list_recordings),
                )
                .route(
                    "/calls/{call_id}/recordings/start",
                    web::post().to(start_recording),
                )
                .route(
                    "/calls/{call_id}/recordings/{recording_id}/stop",
                    web::post().to(stop_recording),
                )
                .route(
                    "/calls/{call_id}/recordings/{recording_id}/attach",
                    web::post().to(attach_recording),
                )
                .route("/call-logs", web::get().to(list_call_logs))
                .route("/ws", web::get().to(phone_ws))
                .route("/queues", web::get().to(list_queues))
                .route("/queues", web::post().to(create_queue))
                .route("/queues/{queue_id}", web::get().to(get_queue))
                .route("/queues/{queue_id}", web::patch().to(update_queue))
                .route("/queues/{queue_id}", web::delete().to(delete_queue))
                .route(
                    "/queues/{queue_id}/members",
                    web::get().to(list_queue_members),
                )
                .route(
                    "/queues/{queue_id}/members",
                    web::post().to(add_queue_member),
                )
                .route(
                    "/queues/{queue_id}/members/{member_id}",
                    web::delete().to(remove_queue_member),
                )
                .route("/queues/{queue_id}/join", web::post().to(join_queue))
                .route(
                    "/queues/{queue_id}/waiting",
                    web::get().to(list_queue_waiting),
                )
                .route("/queues/{queue_id}/answer", web::post().to(answer_queue))
                .route("/admin/queue", web::get().to(admin_queue_dashboard))
                .route("/routing-rules", web::get().to(list_routing_rules))
                .route("/routing-rules", web::post().to(create_routing_rule))
                .route("/routing-rules/evaluate", web::post().to(evaluate_routing))
                .route("/routing-rules/{rule_id}", web::get().to(get_routing_rule))
                .route(
                    "/routing-rules/{rule_id}",
                    web::patch().to(update_routing_rule),
                )
                .route(
                    "/routing-rules/{rule_id}",
                    web::delete().to(delete_routing_rule),
                )
                .route("/presence", web::get().to(list_presence))
                .route("/presence", web::post().to(set_presence))
                .route("/presence/heartbeat", web::post().to(heartbeat_presence))
                .route("/presence/{user_id}", web::get().to(get_presence))
                .route("/ivr-menus", web::get().to(list_ivr_menus))
                .route("/ivr-menus", web::post().to(create_ivr_menu))
                .route("/ivr-menus/{menu_id}", web::get().to(get_ivr_menu))
                .route("/ivr-menus/{menu_id}", web::patch().to(update_ivr_menu))
                .route("/ivr-menus/{menu_id}", web::delete().to(delete_ivr_menu))
                .route("/ivr-menus/{menu_id}/nodes", web::get().to(list_ivr_nodes))
                .route("/ivr-menus/{menu_id}/nodes", web::post().to(add_ivr_node))
                .route(
                    "/ivr-menus/{menu_id}/nodes/{node_id}",
                    web::delete().to(delete_ivr_node),
                )
                .route("/ivr-menus/{menu_id}/resolve", web::post().to(resolve_ivr))
                .route("/auto-response-scripts", web::get().to(list_scripts))
                .route("/auto-response-scripts", web::post().to(create_script))
                .route(
                    "/auto-response-scripts/{script_id}",
                    web::get().to(get_script),
                )
                .route(
                    "/auto-response-scripts/{script_id}",
                    web::patch().to(update_script),
                )
                .route(
                    "/auto-response-scripts/{script_id}",
                    web::delete().to(delete_script),
                )
                .route("/auto-response", web::post().to(auto_response))
                .route("/ai/{capability}", web::post().to(ai_capability))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/phone/calls", owner: "phone", description: "List calls (paginated, filter by status, kind, user, queue, date range; callers see only their own calls).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls", owner: "phone", description: "Initiate a 1:1 or group voice/video call (ringing; callees are notified).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/calls/{call_id}", owner: "phone", description: "Get one call with its participants and recordings (caller, participant, or manager).", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/calls/{call_id}", owner: "phone", description: "Update a live call (subject, recording consent) — caller or manager.", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/calls/{call_id}", owner: "phone", description: "Delete an ended or missed call with its participants, signals, and recordings (caller or manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/accept", owner: "phone", description: "Accept a ringing call (ringing -> active; the call starts).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/reject", owner: "phone", description: "Reject a ringing call; a 1:1 call ends as declined, a group call keeps ringing for the rest.", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/end", owner: "phone", description: "End a call; a call that never connected ends as missed, a connected call as completed with its duration.", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/hold", owner: "phone", description: "Hold an active call (active -> held).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/resume", owner: "phone", description: "Resume a held call (held -> active).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/invite", owner: "phone", description: "Invite another user into a live group call.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/calls/{call_id}/participants", owner: "phone", description: "List the participants of a call (paginated, filter by status).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/signal", owner: "phone", description: "Send one WebRTC signalling message (offer, answer, ICE candidate) in a call; stored and broadcast on the shared WebSocket hub.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/calls/{call_id}/signals", owner: "phone", description: "The signals inbox: stored signalling messages of a call for the caller (paginated, filter by kind and undelivered); reading marks them delivered.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/calls/{call_id}/summary", owner: "phone", description: "Read the AI summary stored on a call log entry.", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/summarize", owner: "phone", description: "Summarize an ended call with the summarize_call AI capability and store the summary on the call log.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/calls/{call_id}/recordings", owner: "phone", description: "List recordings of a call (paginated, filter by status).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/recordings/start", owner: "phone", description: "Start a recording on a live call (consent-gated: the call must carry recording_consent).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/recordings/{recording_id}/stop", owner: "phone", description: "Stop a recording.", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/calls/{call_id}/recordings/{recording_id}/attach", owner: "phone", description: "Attach a stored recording object (storage target + key) to a stopped recording; bytes live in the storage layer.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/call-logs", owner: "phone", description: "Call log: ended and missed calls with caller/callee, started/ended times, duration, direction, and outcome (paginated, filter by user, status, outcome, date range).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/ws", owner: "phone", description: "WebSocket feed of one call's events (?call={id}); also accepts signalling messages.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/queues", owner: "phone", description: "List call queues (paginated, filter by active, search by code/label).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/queues", owner: "phone", description: "Create a call queue as data (manager): ring strategy, ring timeout, max wait, overflow queue and action.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/queues/{queue_id}", owner: "phone", description: "Get one queue with its members, waiting count, and ring order.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/queues/{queue_id}", owner: "phone", description: "Update a call queue (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/queues/{queue_id}", owner: "phone", description: "Delete a queue with no waiting callers and no live calls (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/queues/{queue_id}/members", owner: "phone", description: "List the agents of a queue (paginated, filter by active).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/queues/{queue_id}/members", owner: "phone", description: "Add an agent to a queue with a ring position (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/queues/{queue_id}/members/{member_id}", owner: "phone", description: "Remove an agent from a queue (manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/queues/{queue_id}/join", owner: "phone", description: "Join a queue as a waiting caller.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/queues/{queue_id}/waiting", owner: "phone", description: "List the callers waiting in a queue (managers see all; a waiting caller sees only their own place in line).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/queues/{queue_id}/answer", owner: "phone", description: "Admin answers the next waiting caller: the oldest waiter becomes a ringing call with the admin (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/admin/queue", owner: "phone", description: "Admin queue dashboard: every queue with its waiting callers, member counts, and ring order (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/routing-rules", owner: "phone", description: "List routing rules (paginated, filter by active and target type).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/routing-rules", owner: "phone", description: "Create a routing rule as data (manager): caller pattern, time window, priority, target queue/user/IVR menu.", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/routing-rules/evaluate", owner: "phone", description: "Evaluate the tenant's routing rules for one caller id at a time of day; returns the first matching rule or null.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/routing-rules/{rule_id}", owner: "phone", description: "Get one routing rule.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/routing-rules/{rule_id}", owner: "phone", description: "Update a routing rule (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/routing-rules/{rule_id}", owner: "phone", description: "Delete a routing rule (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/presence", owner: "phone", description: "List presence of the tenant's users (paginated, filter by status; stale entries read as offline).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/presence", owner: "phone", description: "Set the caller's own presence (available, busy, do_not_disturb, offline).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/presence/heartbeat", owner: "phone", description: "Presence heartbeat: refreshes the caller's last-seen time without changing the declared status.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/presence/{user_id}", owner: "phone", description: "Get one user's presence (no row reads as offline).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/ivr-menus", owner: "phone", description: "List IVR menus (paginated, filter by active, search by code/label).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/ivr-menus", owner: "phone", description: "Create an IVR menu as data with its key-press nodes (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/ivr-menus/{menu_id}", owner: "phone", description: "Get one IVR menu with its nodes.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/ivr-menus/{menu_id}", owner: "phone", description: "Update an IVR menu (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/ivr-menus/{menu_id}", owner: "phone", description: "Delete an IVR menu with its nodes (manager).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/ivr-menus/{menu_id}/nodes", owner: "phone", description: "List the nodes of an IVR menu (paginated).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/ivr-menus/{menu_id}/nodes", owner: "phone", description: "Add a key-press node to an IVR menu (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/ivr-menus/{menu_id}/nodes/{node_id}", owner: "phone", description: "Delete a key-press node from an IVR menu (manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/ivr-menus/{menu_id}/resolve", owner: "phone", description: "Resolve one IVR key press: returns the node's action and target (an unknown key repeats the menu).", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/auto-response-scripts", owner: "phone", description: "List auto-response scripts (paginated, filter by active, search by code/label/trigger).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/auto-response-scripts", owner: "phone", description: "Create an auto-response script as data (manager): trigger, response template, or AI capability.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/auto-response-scripts/{script_id}", owner: "phone", description: "Get one auto-response script.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/auto-response-scripts/{script_id}", owner: "phone", description: "Update an auto-response script (manager).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/phone/auto-response-scripts/{script_id}", owner: "phone", description: "Delete an auto-response script (manager).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/auto-response", owner: "phone", description: "Answer a routine caller question: matching script template first, otherwise the answer_caller_question AI capability (AI never books, charges, or transfers).", auth },
            RouteInfo { method: "POST", path: "/api/v1/phone/ai/{capability}", owner: "phone", description: "Run one registered phone AI capability through the AI module's gated runner.", auth },
            RouteInfo { method: "GET", path: "/api/v1/phone/settings", owner: "phone", description: "Get per-tenant phone settings.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/phone/settings", owner: "phone", description: "Update per-tenant phone settings, including the recording-consent gate (manager; defaults to consent required).", auth },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PhoneModule::new()));
}
