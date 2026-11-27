//! chat - module.

// Chat module.
//
// Real-time messaging: channels (public, private, direct messages),
// messages with reactions, file registrations, presence, and a WebSocket
// hub backed by a tokio broadcast channel.
//
// Routes live under `/api/v1/chat`:
//   - `GET/POST  /channels` - list visible channels / create.
//   - `GET/PATCH/DELETE /channels/{id}`.
//   - `POST /channels/{id}/members`, `DELETE /channels/{id}/members/{user_id}`.
//   - `GET/POST  /channels/{id}/messages` - newest first, paginated.
//   - `PATCH/DELETE /channels/{id}/messages/{message_id}`.
//   - `POST /channels/{id}/messages/{message_id}/reactions` (toggle).
//   - `POST /channels/{id}/messages/{message_id}/escalate` - turn a
//     message into a support ticket via the `ticket.create_requested` hook.
//   - `GET/POST  /channels/{id}/files` - register a shared file.
//   - `GET/POST  /dm` - list / find-or-create a 1:1 channel.
//   - `GET/POST  /presence` - presence with last-seen timestamps.
//   - `GET /ws?channel={id}` - WebSocket feed of broadcast chat events.
//
// Live support chat lives under `/api/v1/chat/support`:
//   - `GET/PUT /support/widget/config` - website widget configuration.
//   - `POST    /support/widget/sessions` - visitor bootstrap (creates a
//     backing private channel and routes the session to an agent).
//   - `GET     /support/widget/sessions/{id}` - visitor polling.
//   - `GET/POST /support/sessions` - list / staff-create a session.
//   - `GET     /support/sessions/{id}`.
//   - `POST    /support/sessions/{id}/accept|close|transfer|escalate`.
//   - `GET     /support/sessions/{id}/transcript?format=json|text`.
//   - `GET     /support/sessions/{id}/position` - queue position.
//   - `POST    /support/sessions/{id}/rating`, `GET .../rating`.
//   - `GET     /support/ratings` - all ratings, paginated.
//   - `GET     /support/queue` - waiting visitors.
//   - `GET/POST /support/agents`, `GET/PATCH/DELETE /support/agents/{user_id}`.
//   - `GET/POST /support/canned`, `GET/PATCH/DELETE /support/canned/{id}`,
//     `POST /support/canned/{id}/use`.
//   - `GET/POST /support/offline-messages` - offline message to ticket.
//
// Visitor-facing widget endpoints (`/support/widget/*` bootstrap and
// polling, offline messages, session rating) deliberately do not require
// the `X-User-Id` header: website visitors have no account. They are
// addressed by the unguessable session id returned at bootstrap.
//
// Department routing: a support session carries a `department_id` (an
// opaque identifier owned by the Departments module). Chat asks the
// Departments module for the department's agents through the real
// `department.agents_requested` event (payload `{"department_id": ...}`,
// answer: a result carrying an `agent_ids` array - see
// `tickets.rs`, `agents_for_department`), then picks the least busy,
// effectively available agent among them using presence plus the
// agent's own availability toggle and concurrent-chat limit. When the
// Departments module is not registered yet, chat falls back to its own
// agent roster (agents registered per department under
// `/support/agents`). Agents the Departments module reports but who
// have no chat roster record are treated as available with the
// default concurrent-chat limit.
//
// Canned responses: no other module in this tree owns canned responses
// (the tickets module has none), so chat is the canonical store. The
// tickets module - or any other module - can reuse them by listening to
// `chat.canned_response.created/updated/deleted` or by reading them
// through `GET /api/v1/chat/support/canned`.
//
// File sharing: a shared file is only a registration here (name, size,
// mime plus an optional storage reference). The bytes live in the
// storage module; `storage_target` + `storage_key` identify the object
// and `url` carries a public or signed URL when the uploader has one.
//
// Actor identity: until the auth module provides an extractor, the caller
// is identified by the `X-User-Id` header (required). Role checks use the
// optional `X-User-Roles` header (comma separated) as an interim source
// until the roles module exposes a lookup hook; role-restricted channels
// only admit members when no roles header is supplied.
//
// SECURITY: role-restricted channels are NOT a security boundary.
// `X-User-Roles` is self-asserted - any caller can claim `admin`
// and pass `can_view` on a role-restricted public channel - and
// the roles module offers no server-side user-to-roles lookup to
// replace it with (only the `role.check` permission hook, which
// checks a role slug, not a user). Treat a role-restricted public
// channel as a convenience filter only; sensitive conversations
// belong in private or DM channels, where membership decides
// visibility. See `caller_roles` in `helpers.rs`.
//
// Events emitted on the hook bus:
//   - `chat.channel.created`, `chat.channel.updated`, `chat.channel.deleted`
//   - `chat.message` (on every new message)
//   - `file.shared` (on file registration, consumed by the File Manager)
//   - `notification.requested` (once per `@user` mention)
//   - `ticket.create_requested` (on message escalation, chat-to-ticket
//     escalation, and offline messages; answered by the tickets module)
//   - `chat.session.created`, `chat.session.queued`, `chat.session.assigned`,
//     `chat.session.accepted`, `chat.session.transferred`,
//     `chat.session.closed`, `chat.session.escalated`, `chat.session.rated`
//   - `chat.canned_response.created`, `chat.canned_response.updated`,
//     `chat.canned_response.deleted`, `chat.canned_response.used`
//   - `chat.offline_message.created`
//
// Events consumed from other modules:
//   - `department.agents_requested` (asked when routing a session; the
//     Departments module answers with an `agent_ids` result)

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::{limits::*, types::*, hub::*, state::*, helpers::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};
use super::ai_capabilities::AI_CAPABILITIES;



// Module
pub struct ChatModule {
    pub(crate) state: ChatState,
}


impl ChatModule {
    pub fn new() -> Self {
        Self {
            state: ChatState::default(),
        }
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ChatModule::new()));
}


impl Module for ChatModule {
    fn id(&self) -> &'static str {
        "chat"
    }

    fn name(&self) -> &'static str {
        "Chat"
    }

    fn description(&self) -> &'static str {
        "Real-time messaging: channels, direct messages, reactions, file sharing, presence, a WebSocket hub, and live support chat with widget, department routing, queues, canned responses, transcripts, ratings, transfers, and ticket escalation."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company", "notifications", "tickets", "storage"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("chat", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/chat/channels", owner: "chat", description: "List channels", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels", owner: "chat", description: "Create channel", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Get channel", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Update channel", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Delete channel", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/members", owner: "chat", description: "Add member", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}/members/{user_id}", owner: "chat", description: "Remove member", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}/messages", owner: "chat", description: "List messages", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages", owner: "chat", description: "Post message", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/chat/channels/{id}/messages/{message_id}", owner: "chat", description: "Update message", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}/messages/{message_id}", owner: "chat", description: "Delete message", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages/{message_id}/reactions", owner: "chat", description: "Toggle reaction", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages/{message_id}/escalate", owner: "chat", description: "Escalate message", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}/files", owner: "chat", description: "List files", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/files", owner: "chat", description: "Share file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/dm", owner: "chat", description: "List dms", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/dm", owner: "chat", description: "Create dm", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/presence", owner: "chat", description: "List presence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/presence", owner: "chat", description: "Set presence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/ws", owner: "chat", description: "Chat ws", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/widget/config", owner: "chat", description: "Get widget config", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/chat/support/widget/config", owner: "chat", description: "Save widget config", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/widget/sessions", owner: "chat", description: "Bootstrap widget session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/widget/sessions/{id}", owner: "chat", description: "Poll widget session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions", owner: "chat", description: "List support sessions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions", owner: "chat", description: "Create support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}", owner: "chat", description: "Get support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/accept", owner: "chat", description: "Accept support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/close", owner: "chat", description: "Close support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/transfer", owner: "chat", description: "Transfer support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/escalate", owner: "chat", description: "Escalate session to ticket", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/transcript", owner: "chat", description: "Get session transcript", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/position", owner: "chat", description: "Get queue position", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/rating", owner: "chat", description: "Rate support session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/rating", owner: "chat", description: "Get session rating", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/ratings", owner: "chat", description: "List chat ratings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/queue", owner: "chat", description: "List waiting visitors", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/agents", owner: "chat", description: "List support agents", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/agents", owner: "chat", description: "Register support agent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Get support agent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Update support agent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Remove support agent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/canned", owner: "chat", description: "List canned responses", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/canned", owner: "chat", description: "Create canned response", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Get canned response", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Update canned response", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Delete canned response", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/canned/{id}/use", owner: "chat", description: "Use canned response", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/chat/support/offline-messages", owner: "chat", description: "List offline messages", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/chat/support/offline-messages", owner: "chat", description: "Create offline message", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/chat")
                .app_data(state)
                .route("/channels", web::get().to(list_channels))
                .route("/channels", web::post().to(create_channel))
                .route("/channels/{id}", web::get().to(get_channel))
                .route("/channels/{id}", web::patch().to(update_channel))
                .route("/channels/{id}", web::delete().to(delete_channel))
                .route("/channels/{id}/members", web::post().to(add_member))
                .route(
                    "/channels/{id}/members/{user_id}",
                    web::delete().to(remove_member),
                )
                .route("/channels/{id}/messages", web::get().to(list_messages))
                .route("/channels/{id}/messages", web::post().to(post_message))
                .route(
                    "/channels/{id}/messages/{message_id}",
                    web::patch().to(update_message),
                )
                .route(
                    "/channels/{id}/messages/{message_id}",
                    web::delete().to(delete_message),
                )
                .route(
                    "/channels/{id}/messages/{message_id}/reactions",
                    web::post().to(toggle_reaction),
                )
                .route(
                    "/channels/{id}/messages/{message_id}/escalate",
                    web::post().to(escalate_message),
                )
                .route("/channels/{id}/files", web::get().to(list_files))
                .route("/channels/{id}/files", web::post().to(share_file))
                .route("/dm", web::get().to(list_dms))
                .route("/dm", web::post().to(create_dm))
                .route("/presence", web::get().to(list_presence))
                .route("/presence", web::post().to(set_presence))
                .route("/ws", web::get().to(chat_ws))
                .route("/support/widget/config", web::get().to(get_widget_config))
                .route("/support/widget/config", web::put().to(save_widget_config))
                .route(
                    "/support/widget/sessions",
                    web::post().to(bootstrap_widget_session),
                )
                .route(
                    "/support/widget/sessions/{id}",
                    web::get().to(poll_widget_session),
                )
                .route("/support/sessions", web::get().to(list_support_sessions))
                .route("/support/sessions", web::post().to(create_support_session))
                .route("/support/sessions/{id}", web::get().to(get_support_session))
                .route(
                    "/support/sessions/{id}/accept",
                    web::post().to(accept_support_session),
                )
                .route(
                    "/support/sessions/{id}/close",
                    web::post().to(close_support_session),
                )
                .route(
                    "/support/sessions/{id}/transfer",
                    web::post().to(transfer_support_session),
                )
                .route(
                    "/support/sessions/{id}/escalate",
                    web::post().to(escalate_support_session),
                )
                .route(
                    "/support/sessions/{id}/transcript",
                    web::get().to(get_session_transcript),
                )
                .route(
                    "/support/sessions/{id}/position",
                    web::get().to(get_queue_position),
                )
                .route(
                    "/support/sessions/{id}/rating",
                    web::post().to(rate_support_session),
                )
                .route(
                    "/support/sessions/{id}/rating",
                    web::get().to(get_session_rating),
                )
                .route("/support/ratings", web::get().to(list_ratings))
                .route("/support/queue", web::get().to(list_queue))
                .route("/support/agents", web::get().to(list_agents))
                .route("/support/agents", web::post().to(register_agent))
                .route("/support/agents/{user_id}", web::get().to(get_agent))
                .route("/support/agents/{user_id}", web::patch().to(update_agent))
                .route("/support/agents/{user_id}", web::delete().to(remove_agent))
                .route("/support/canned", web::get().to(list_canned))
                .route("/support/canned", web::post().to(create_canned))
                .route("/support/canned/{id}", web::get().to(get_canned))
                .route("/support/canned/{id}", web::patch().to(update_canned))
                .route("/support/canned/{id}", web::delete().to(delete_canned))
                .route("/support/canned/{id}/use", web::post().to(use_canned))
                .route(
                    "/support/offline-messages",
                    web::get().to(list_offline_messages),
                )
                .route(
                    "/support/offline-messages",
                    web::post().to(create_offline_message),
                ),
        );
    }
}
