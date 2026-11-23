//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::ai_capability;
use super::handlers_campaigns::{
    cancel_campaign, create_campaign, delete_campaign, get_campaign, list_campaigns,
    pause_campaign, schedule_campaign, send_campaign_now, update_campaign,
};
use super::handlers_lists::{
    create_contact, create_contact_list, delete_contact, delete_contact_list, get_contact_list,
    list_contact_lists, list_contacts, update_contact_list,
};
use super::handlers_messages::{get_message, list_messages, retry_message};
use super::handlers_optouts::{
    create_keyword, create_opt_out, delete_keyword, delete_opt_out, delivery_webhook,
    inbound_webhook, list_keywords, list_opt_outs, update_keyword,
};
use super::handlers_providers::{
    create_provider, delete_provider, get_provider, list_providers, rotate_provider_webhook_secret,
    test_provider, update_provider,
};
use super::handlers_settings::{get_rate_limit, list_audit, update_rate_limit};
use super::handlers_templates::{
    create_template, delete_template, get_template, list_templates, preview_template,
    update_template,
};
use super::hooks::{
    ai_readiness_event, ai_summarize_event, on_ai_readiness_requested, on_ai_summarize_requested,
    on_marketing_campaign_sent, on_user_deleted,
};
use super::state::SmsMarketingState;
use super::types::{EVENT_MARKETING_CAMPAIGN_SENT, EVENT_USER_DELETED};
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

pub struct SmsMarketingModule {
    pub(crate) state: SmsMarketingState,
}

impl SmsMarketingModule {
    pub fn new() -> Self {
        Self {
            state: SmsMarketingState::default(),
        }
    }
}

impl Default for SmsMarketingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SmsMarketingModule {
    fn id(&self) -> &'static str {
        "sms_marketing"
    }

    fn name(&self) -> &'static str {
        "SMS Marketing"
    }

    fn description(&self) -> &'static str {
        "SMS marketing: admin-configurable SMS providers as data (generic HTTP adapter posting JSON to the configured endpoint with the configured auth, plus a log-only test provider; credentials write-only and encrypted, webhook secrets hash-stored and returned once), plain-text templates with {{variable}} substitution and GSM-7 / UCS-2 segment counting, contact lists with tags and per-contact variables, campaigns with a draft / scheduled / sending / sent / paused / cancelled status machine and audience selection by list reference plus data segment filters plus explicit numbers, a send pass that blocks opted-out numbers and enforces per-tenant and per-provider per-minute limits at enqueue time (over-limit messages stay queued, never dropped), per-message delivery tracking with a shared-secret delivery webhook and an inbound keyword webhook, opt-out keywords as data with a per-tenant opt-out list, and an append-only audit trail."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("sms_marketing", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "sms_marketing",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CAMPAIGN_SENT,
            "sms_marketing",
            Arc::new(move |ctx: &mut HookContext| on_marketing_campaign_sent(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "sms_marketing",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_readiness_event(),
            "sms_marketing",
            Arc::new(move |ctx: &mut HookContext| on_ai_readiness_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/sms-marketing")
                .app_data(state)
                .route("/providers", web::get().to(list_providers))
                .route("/providers", web::post().to(create_provider))
                .route("/providers/{provider_id}", web::get().to(get_provider))
                .route("/providers/{provider_id}", web::patch().to(update_provider))
                .route(
                    "/providers/{provider_id}",
                    web::delete().to(delete_provider),
                )
                .route(
                    "/providers/{provider_id}/rotate-webhook-secret",
                    web::post().to(rotate_provider_webhook_secret),
                )
                .route(
                    "/providers/{provider_id}/test",
                    web::post().to(test_provider),
                )
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{template_id}", web::get().to(get_template))
                .route("/templates/{template_id}", web::patch().to(update_template))
                .route(
                    "/templates/{template_id}",
                    web::delete().to(delete_template),
                )
                .route(
                    "/templates/{template_id}/preview",
                    web::post().to(preview_template),
                )
                .route("/contact-lists", web::get().to(list_contact_lists))
                .route("/contact-lists", web::post().to(create_contact_list))
                .route("/contact-lists/{list_id}", web::get().to(get_contact_list))
                .route(
                    "/contact-lists/{list_id}",
                    web::patch().to(update_contact_list),
                )
                .route(
                    "/contact-lists/{list_id}",
                    web::delete().to(delete_contact_list),
                )
                .route(
                    "/contact-lists/{list_id}/contacts",
                    web::get().to(list_contacts),
                )
                .route(
                    "/contact-lists/{list_id}/contacts",
                    web::post().to(create_contact),
                )
                .route(
                    "/contact-lists/{list_id}/contacts/{contact_id}",
                    web::delete().to(delete_contact),
                )
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{campaign_id}", web::get().to(get_campaign))
                .route("/campaigns/{campaign_id}", web::patch().to(update_campaign))
                .route(
                    "/campaigns/{campaign_id}",
                    web::delete().to(delete_campaign),
                )
                .route(
                    "/campaigns/{campaign_id}/schedule",
                    web::post().to(schedule_campaign),
                )
                .route(
                    "/campaigns/{campaign_id}/cancel",
                    web::post().to(cancel_campaign),
                )
                .route(
                    "/campaigns/{campaign_id}/pause",
                    web::post().to(pause_campaign),
                )
                .route(
                    "/campaigns/{campaign_id}/send-now",
                    web::post().to(send_campaign_now),
                )
                .route("/messages", web::get().to(list_messages))
                .route("/messages/{message_id}", web::get().to(get_message))
                .route(
                    "/messages/{message_id}/retry",
                    web::post().to(retry_message),
                )
                .route("/opt-outs", web::get().to(list_opt_outs))
                .route("/opt-outs", web::post().to(create_opt_out))
                .route("/opt-outs/{opt_out_id}", web::delete().to(delete_opt_out))
                .route("/opt-out-keywords", web::get().to(list_keywords))
                .route("/opt-out-keywords", web::post().to(create_keyword))
                .route(
                    "/opt-out-keywords/{keyword_id}",
                    web::patch().to(update_keyword),
                )
                .route(
                    "/opt-out-keywords/{keyword_id}",
                    web::delete().to(delete_keyword),
                )
                .route("/settings/rate-limit", web::get().to(get_rate_limit))
                .route("/settings/rate-limit", web::put().to(update_rate_limit))
                .route(
                    "/webhooks/delivery/{provider_id}",
                    web::post().to(delivery_webhook),
                )
                .route(
                    "/webhooks/inbound/{provider_id}",
                    web::post().to(inbound_webhook),
                )
                .route("/audit", web::get().to(list_audit))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        let public = unified_core::auth::RouteAuth::Public;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/providers",
                owner: "sms_marketing",
                description: "List SMS providers (paginated, filter by kind, auth type, enabled; search by name/sender id; masked view — no credential).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/providers",
                owner: "sms_marketing",
                description: "Create an SMS provider with kind, HTTPS endpoint, auth type, write-only encrypted credential, sender id, and per-provider rate limit; the raw webhook secret is returned once (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/providers/{provider_id}",
                owner: "sms_marketing",
                description: "Get one SMS provider (masked view — the credential and webhook secret never appear).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sms-marketing/providers/{provider_id}",
                owner: "sms_marketing",
                description: "Update an SMS provider; a supplied credential or webhook secret replaces the stored one, the new webhook secret returned once (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/providers/{provider_id}",
                owner: "sms_marketing",
                description: "Delete an SMS provider; refused while any campaign references it (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/providers/{provider_id}/rotate-webhook-secret",
                owner: "sms_marketing",
                description: "Rotate a provider's webhook secret; the fresh raw secret is returned once (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/providers/{provider_id}/test",
                owner: "sms_marketing",
                description: "Dry-run check of a provider's configuration (endpoint, auth, stored credential readability). Makes no network call and sends no message (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/templates",
                owner: "sms_marketing",
                description: "List plain-text SMS templates (paginated, search by name/body).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/templates",
                owner: "sms_marketing",
                description: "Create a plain-text SMS template with {{variable}} placeholders (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/templates/{template_id}",
                owner: "sms_marketing",
                description: "Get one SMS template.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sms-marketing/templates/{template_id}",
                owner: "sms_marketing",
                description: "Update an SMS template; sent messages keep their rendered body (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/templates/{template_id}",
                owner: "sms_marketing",
                description: "Delete an SMS template; refused while any campaign references it (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/templates/{template_id}/preview",
                owner: "sms_marketing",
                description: "Preview a template with variables: rendered body, missing variables, and the GSM-7 / UCS-2 segment count.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/contact-lists",
                owner: "sms_marketing",
                description: "List contact lists (paginated, search by name; each row carries its contact count).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/contact-lists",
                owner: "sms_marketing",
                description: "Create a contact list (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}",
                owner: "sms_marketing",
                description: "Get one contact list with its contact count.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}",
                owner: "sms_marketing",
                description: "Update a contact list (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}",
                owner: "sms_marketing",
                description: "Delete a contact list with its contacts; campaigns that referenced it skip the missing list at send time (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}/contacts",
                owner: "sms_marketing",
                description: "List contacts of one list (paginated, filter by tag; search by number / display name).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}/contacts",
                owner: "sms_marketing",
                description: "Add a contact to a list: normalized E.164 number, tags, and template variables; one number per list (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/contact-lists/{list_id}/contacts/{contact_id}",
                owner: "sms_marketing",
                description: "Delete a contact from its list (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/campaigns",
                owner: "sms_marketing",
                description: "List campaigns (paginated, filter by status, provider, template; search by name).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/campaigns",
                owner: "sms_marketing",
                description: "Create a campaign with template, provider, list references, explicit numbers, a segment filter, and variables; a future send_at starts it as scheduled (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}",
                owner: "sms_marketing",
                description: "Get one campaign with its send counts.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}",
                owner: "sms_marketing",
                description: "Update a campaign; only drafts and cancelled campaigns are editable (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}",
                owner: "sms_marketing",
                description: "Delete a campaign; only drafts and cancelled campaigns can be deleted, its message rows are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}/schedule",
                owner: "sms_marketing",
                description: "Schedule a campaign for a future send time; emits sms.campaign.scheduled for a scheduler to pick up (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}/cancel",
                owner: "sms_marketing",
                description: "Cancel a campaign; messages already sent stay sent (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}/pause",
                owner: "sms_marketing",
                description: "Pause a scheduled or sending campaign (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/campaigns/{campaign_id}/send-now",
                owner: "sms_marketing",
                description: "Send a campaign now: resolves the audience, renders per recipient, blocks opted-out numbers, queues over-limit recipients, and dispatches the rest through the provider adapter (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/messages",
                owner: "sms_marketing",
                description: "List messages (paginated, filter by provider, status, campaign; search by phone number / provider message id).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/messages/{message_id}",
                owner: "sms_marketing",
                description: "Get one message.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/messages/{message_id}/retry",
                owner: "sms_marketing",
                description: "Retry a failed, undelivered, or rate-limited queued message; opt-out and rate-limit checks run again (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/opt-outs",
                owner: "sms_marketing",
                description: "List opted-out numbers (paginated, filter by source; search by phone number).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/opt-outs",
                owner: "sms_marketing",
                description: "Add a number to the opt-out list by hand; sends to it are blocked from the next send pass (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/opt-outs/{opt_out_id}",
                owner: "sms_marketing",
                description: "Remove a number from the opt-out list (re-opt-in); sends to it are allowed again (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/opt-out-keywords",
                owner: "sms_marketing",
                description: "List opt-out / opt-in keyword rows (paginated, filter by kind, enabled); the first read seeds the default keywords as data rows.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/opt-out-keywords",
                owner: "sms_marketing",
                description: "Create an opt-out or opt-in keyword row (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sms-marketing/opt-out-keywords/{keyword_id}",
                owner: "sms_marketing",
                description: "Update an opt-out / opt-in keyword row (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sms-marketing/opt-out-keywords/{keyword_id}",
                owner: "sms_marketing",
                description: "Delete an opt-out / opt-in keyword row (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/settings/rate-limit",
                owner: "sms_marketing",
                description: "Get the tenant's per-minute send limit (the default applies when no row exists).",
                auth,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/sms-marketing/settings/rate-limit",
                owner: "sms_marketing",
                description: "Set the tenant's per-minute send limit; zero blocks every send (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/webhooks/delivery/{provider_id}",
                owner: "sms_marketing",
                description: "PUBLIC (shared-secret gated): provider delivery-status callback — updates one message to sent / delivered / failed / undelivered.",
                auth: public,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/webhooks/inbound/{provider_id}",
                owner: "sms_marketing",
                description: "PUBLIC (shared-secret gated): provider inbound-message callback — matches the body against the keyword rows and opts the sender out or back in.",
                auth: public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sms-marketing/audit",
                owner: "sms_marketing",
                description: "List the audit trail (paginated, filter by entity type, entity, action, actor; newest first).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sms-marketing/ai/{capability}",
                owner: "sms_marketing",
                description: "Run one registered SMS marketing AI capability through the AI module's gated runner.",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SmsMarketingModule::new()));
}
