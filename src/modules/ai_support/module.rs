// AI Support module.
//
// A support assistant whose answers are grounded in real knowledgebase
// articles: every question is sent to the `knowledgebase.search` hook and
// the answer is built only from the articles that hook returns. When no
// article matches, the assistant says so honestly and offers a handoff
// instead of fabricating an answer.
//
// Routes live under `/api/v1/ai-support`:
//   - `POST /ask {message, conversation_id?}` — grounded answer.
//   - `GET  /conversations` — paginated conversation history.
//   - `GET/PATCH/DELETE /conversations/{id}`.
//   - `POST /handoff {conversation_id}` — request a support ticket.
//   - `POST /tickets {conversation_id, summary}` — same, with a summary.
//   - `GET/POST  /connectors`, `GET/PATCH/DELETE /connectors/{id}` —
//     external chat connector registry (webchat, telegram, whatsapp, ...).
//
// ERP interconnection (all through hooks, never direct module calls):
//   - Grounding: emits `knowledgebase.search`, reads the returned
//     articles from the hook results.
//   - Handoff: emits `ticket.create_requested`; the tickets module
//     creates the ticket and may return its id in the hook results.
//
// Connector note: enabling a connector stores its configuration. Making
// a live connection to the external chat service is a documented next
// milestone, not part of this module (see the TODO on `Connector`).
//
// Connector authorization: connectors are owned per user, like
// conversations. Every connector operation requires the
// `ai-support.connectors.manage` capability via the `role.check`
// hook (fail closed) plus `X-User-Id` / `X-Role` headers, and only
// the owner — or a platform admin (`admin` / `super_admin` role) —
// may read, update or delete one. The config is stored verbatim
// but every response carries a redacted copy with secret-bearing
// fields (tokens, API keys, secrets, passwords) masked.
//
// Conversation lifecycle: `open` -> `handoff` -> `closed`, with
// `closed` -> `open` only as an explicit reopen via PATCH. Asking a
// question on a closed conversation is a 409 and never reopens it;
// any other move is a 409 (see `ConversationStatus::can_transition`
// in `types.rs`).

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AiSupportModule {
    pub(crate) state: AiSupportState,
}

impl AiSupportModule {
    pub fn new() -> Self {
        Self {
            state: AiSupportState::default(),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AiSupportModule::new()));
}

impl Module for AiSupportModule {
    fn id(&self) -> &'static str {
        "ai-support"
    }

    fn name(&self) -> &'static str {
        "AI Support"
    }

    fn description(&self) -> &'static str {
        "AI support assistant grounded in knowledgebase articles, with handoff to human support tickets and an external chat connector registry."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["knowledgebase", "tickets", "chat"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("ai-support", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/ai-support/ask", owner: "ai-support", description: "Ask", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/ai-support/conversations", owner: "ai-support", description: "List conversations", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Get conversation", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Update conversation", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Delete conversation", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/ai-support/handoff", owner: "ai-support", description: "Handoff", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/ai-support/tickets", owner: "ai-support", description: "Create ticket", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/ai-support/connectors", owner: "ai-support", description: "List connectors", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/ai-support/connectors", owner: "ai-support", description: "Create connector", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Get connector", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Update connector", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Delete connector", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/ai-support")
                .app_data(state)
                .route("/ask", web::post().to(ask))
                .route("/conversations", web::get().to(list_conversations))
                .route("/conversations/{id}", web::get().to(get_conversation))
                .route(
                    "/conversations/{id}",
                    web::patch().to(update_conversation),
                )
                .route(
                    "/conversations/{id}",
                    web::delete().to(delete_conversation),
                )
                .route("/handoff", web::post().to(handoff))
                .route("/tickets", web::post().to(create_ticket))
                .route("/connectors", web::get().to(list_connectors))
                .route("/connectors", web::post().to(create_connector))
                .route("/connectors/{id}", web::get().to(get_connector))
                .route("/connectors/{id}", web::patch().to(update_connector))
                .route("/connectors/{id}", web::delete().to(delete_connector)),
        );
    }
}
