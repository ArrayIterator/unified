//! ai module - Module registration: struct, Default, Module impl, register.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use super::capabilities::AI_CAPABILITIES;
use super::capabilities::AI_STATE;
use super::capabilities::register_ai_capabilities;
use super::constants::EVENT_AI_CONTEXT_REQUESTED;
use super::constants::EVENT_SITE_CONTEXT_REQUESTED;
use super::context::assemble_shared_context;
use super::state::AiState;
use super::types::SiteContextInput;
use super::capabilities::{list_capabilities, list_module_capabilities, run_capability_endpoint};
use super::handlers_drafts::{answer, approve_draft, chat_draft, complete, content_draft, delete_draft, email_draft, get_draft, list_drafts, reject_draft, support_draft};
use super::handlers_gating::{get_effective_gate, get_global_settings, patch_company_gate, patch_global_settings, patch_user_gate};
use super::handlers_personal::{create_my_mcp, delete_my_key, delete_my_mcp, get_context, get_my_key, get_my_mcp, list_my_mcp, test_my_key, update_my_mcp, upsert_my_key, usage_by_company, usage_by_user};
use super::handlers_providers::{create_provider_connection, delete_provider_connection, get_provider_connection, list_provider_connections, list_providers, test_provider_connection, update_provider_connection};
use super::handlers_usage::{create_company_policy, create_user_policy, delete_company_policy, delete_user_policy, get_company_policy, get_usage, get_user_policy, list_company_policies, list_usage, list_user_policies, update_company_policy, update_user_policy, usage_summary};

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct AiModule {
    pub(crate) state: AiState,
}

impl AiModule {
    pub fn new() -> Self {
        Self {
            state: AiState::default(),
        }
    }
}

impl Default for AiModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AiModule::new()));
}

impl Module for AiModule {
    fn id(&self) -> &'static str {
        "ai"
    }

    fn name(&self) -> &'static str {
        "AI"
    }

    fn description(&self) -> &'static str {
        "AI provider abstraction (Anthropic, OpenAI, OpenAI-compatible, local) with human-approved drafts for support, content, and email, plus usage tracking, a shared site context, company/user access allowlists with personal API keys and personal MCP servers, layered AI gating (global kill-switch, purchased-product layer, company and user on/off switches, per-feature toggles) enforced on every call, and a registry of per-module AI capabilities run through one gated runner."
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "roles",
            "audit",
            "knowledgebase",
            "tickets",
            "chat",
            "content",
            "site-builder",
            "newsletter",
            "email_providers",
        ]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        // Share this module's state with the capability runner
        // (`run_ai_capability`); the first initialisation wins.
        let _ = AI_STATE.set(self.state.clone());
        // Register the AI module's own capabilities so the registry
        // is self-describing from startup.
        register_ai_capabilities("ai", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer the shared-context request: the same context the AI
        // features use. The handler passes `include_site_hooks:
        // false` so it never re-emits the event it is answering;
        // other modules (including the public server module) answer
        // the same event with their own site facts.
        for event in [EVENT_SITE_CONTEXT_REQUESTED, EVENT_AI_CONTEXT_REQUESTED] {
            let state = self.state.clone();
            bus.register(
                event,
                "ai",
                Arc::new(move |ctx: &mut HookContext| {
                    let query = ctx
                        .get("query")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    let site = SiteContextInput::from_hook(ctx);
                    let hooks = state.hooks.read().expect("hooks lock poisoned").clone();
                    let shared = assemble_shared_context(hooks.as_deref(), &query, &site, false);
                    ctx.add_result(json!({
                        "site_name": shared.site_name,
                        "site_url": shared.site_url,
                        "locale": shared.locale,
                        "grounding_articles": shared.grounding_articles,
                        "extra": shared.extra,
                        "assembled_at": shared.assembled_at,
                    }));
                }),
            );
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/providers",
                owner: "ai",
                description: "List provider kinds",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/provider-connections",
                owner: "ai",
                description: "List provider connections (masked)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/provider-connections",
                owner: "ai",
                description: "Create provider connection",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/provider-connections/{id}",
                owner: "ai",
                description: "Get provider connection (masked)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/provider-connections/{id}",
                owner: "ai",
                description: "Update provider connection",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/provider-connections/{id}",
                owner: "ai",
                description: "Delete provider connection",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/provider-connections/{id}/test",
                owner: "ai",
                description: "Test provider connection",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/complete",
                owner: "ai",
                description: "Run a completion, stored as a draft",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/support-draft",
                owner: "ai",
                description: "Draft a ticket reply (draft only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/chat-draft",
                owner: "ai",
                description: "Draft a chat reply (draft only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/content-draft",
                owner: "ai",
                description: "Draft post/page text (draft only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/email-draft",
                owner: "ai",
                description: "Draft email subject + body (draft only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/answer",
                owner: "ai",
                description: "Knowledgebase-grounded answer (draft only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/drafts",
                owner: "ai",
                description: "List drafts",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/drafts/{id}",
                owner: "ai",
                description: "Get draft",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/drafts/{id}/approve",
                owner: "ai",
                description: "Approve a draft (human review; does not send or publish)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/drafts/{id}/reject",
                owner: "ai",
                description: "Reject a draft",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/drafts/{id}",
                owner: "ai",
                description: "Delete a draft",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/usage",
                owner: "ai",
                description: "List usage records",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/usage/{id}",
                owner: "ai",
                description: "Get usage record",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/usage/summary",
                owner: "ai",
                description: "Usage summary",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/usage/by-user",
                owner: "ai",
                description: "Usage records for one user (own records, or any user for usage readers)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/usage/by-company",
                owner: "ai",
                description: "Usage records for one company (own company, or any company for platform usage readers)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/access/companies",
                owner: "ai",
                description: "List company access policies",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/access/companies",
                owner: "ai",
                description: "Create company access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/access/companies/{id}",
                owner: "ai",
                description: "Get company access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/access/companies/{id}",
                owner: "ai",
                description: "Update company access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/access/companies/{id}",
                owner: "ai",
                description: "Delete company access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/access/users",
                owner: "ai",
                description: "List user access policies",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/access/users",
                owner: "ai",
                description: "Create user access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/access/users/{id}",
                owner: "ai",
                description: "Get user access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/access/users/{id}",
                owner: "ai",
                description: "Update user access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/access/users/{id}",
                owner: "ai",
                description: "Delete user access policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/my-key",
                owner: "ai",
                description: "Get own personal API key metadata (never the key)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/my-key",
                owner: "ai",
                description: "Create or rotate own personal API key",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/my-key",
                owner: "ai",
                description: "Delete own personal API key",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/my-key/test",
                owner: "ai",
                description: "Test own personal API key",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/my-mcp",
                owner: "ai",
                description: "List own personal MCP servers",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/my-mcp",
                owner: "ai",
                description: "Create own personal MCP server",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/my-mcp/{id}",
                owner: "ai",
                description: "Get own personal MCP server",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/my-mcp/{id}",
                owner: "ai",
                description: "Update own personal MCP server",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/ai/my-mcp/{id}",
                owner: "ai",
                description: "Delete own personal MCP server",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/context",
                owner: "ai",
                description: "Get the shared site context",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/capabilities",
                owner: "ai",
                description: "List every registered AI capability",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/capabilities/{module_id}",
                owner: "ai",
                description: "List one module's registered AI capabilities",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/ai/capabilities/{module_id}/{capability}",
                owner: "ai",
                description: "Run one registered AI capability",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/gating/effective",
                owner: "ai",
                description: "Effective layered gate decision for a user / feature, with per-layer breakdown",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/gating/company/{company_id}",
                owner: "ai",
                description: "Switch AI on/off or switch features off for a company",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/gating/user/{user_id}",
                owner: "ai",
                description: "Switch AI on/off or switch features off for a user",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/ai/gating/settings",
                owner: "ai",
                description: "Read the global AI settings (kill-switch + global feature switches)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/ai/gating/settings",
                owner: "ai",
                description: "Change the global AI settings (kill-switch + global feature switches)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/ai")
                .route("/providers", web::get().to(list_providers))
                .route(
                    "/provider-connections",
                    web::get().to(list_provider_connections),
                )
                .route(
                    "/provider-connections",
                    web::post().to(create_provider_connection),
                )
                .route(
                    "/provider-connections/{id}",
                    web::get().to(get_provider_connection),
                )
                .route(
                    "/provider-connections/{id}",
                    web::patch().to(update_provider_connection),
                )
                .route(
                    "/provider-connections/{id}",
                    web::delete().to(delete_provider_connection),
                )
                .route(
                    "/provider-connections/{id}/test",
                    web::post().to(test_provider_connection),
                )
                .route("/complete", web::post().to(complete))
                .route("/support-draft", web::post().to(support_draft))
                .route("/chat-draft", web::post().to(chat_draft))
                .route("/content-draft", web::post().to(content_draft))
                .route("/email-draft", web::post().to(email_draft))
                .route("/answer", web::post().to(answer))
                .route("/drafts", web::get().to(list_drafts))
                .route("/drafts/{id}", web::get().to(get_draft))
                .route("/drafts/{id}/approve", web::post().to(approve_draft))
                .route("/drafts/{id}/reject", web::post().to(reject_draft))
                .route("/drafts/{id}", web::delete().to(delete_draft))
                .route("/usage", web::get().to(list_usage))
                .route("/usage/summary", web::get().to(usage_summary))
                .route("/usage/by-user", web::get().to(usage_by_user))
                .route("/usage/by-company", web::get().to(usage_by_company))
                .route("/usage/{id}", web::get().to(get_usage))
                .route("/access/companies", web::get().to(list_company_policies))
                .route("/access/companies", web::post().to(create_company_policy))
                .route("/access/companies/{id}", web::get().to(get_company_policy))
                .route(
                    "/access/companies/{id}",
                    web::patch().to(update_company_policy),
                )
                .route(
                    "/access/companies/{id}",
                    web::delete().to(delete_company_policy),
                )
                .route("/access/users", web::get().to(list_user_policies))
                .route("/access/users", web::post().to(create_user_policy))
                .route("/access/users/{id}", web::get().to(get_user_policy))
                .route("/access/users/{id}", web::patch().to(update_user_policy))
                .route("/access/users/{id}", web::delete().to(delete_user_policy))
                .route("/my-key", web::get().to(get_my_key))
                .route("/my-key", web::post().to(upsert_my_key))
                .route("/my-key", web::delete().to(delete_my_key))
                .route("/my-key/test", web::post().to(test_my_key))
                .route("/my-mcp", web::get().to(list_my_mcp))
                .route("/my-mcp", web::post().to(create_my_mcp))
                .route("/my-mcp/{id}", web::get().to(get_my_mcp))
                .route("/my-mcp/{id}", web::patch().to(update_my_mcp))
                .route("/my-mcp/{id}", web::delete().to(delete_my_mcp))
                .route("/gating/effective", web::get().to(get_effective_gate))
                .route(
                    "/gating/company/{company_id}",
                    web::patch().to(patch_company_gate),
                )
                .route("/gating/user/{user_id}", web::patch().to(patch_user_gate))
                .route("/gating/settings", web::get().to(get_global_settings))
                .route("/gating/settings", web::patch().to(patch_global_settings))
                .route("/context", web::get().to(get_context))
                .route("/capabilities", web::get().to(list_capabilities))
                .route(
                    "/capabilities/{module_id}",
                    web::get().to(list_module_capabilities),
                )
                .route(
                    "/capabilities/{module_id}/{capability}",
                    web::post().to(run_capability_endpoint),
                ),
        );
    }
}
