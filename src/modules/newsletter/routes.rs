// Newsletter module - module registration and routes.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::campaigns::{campaign_stats, create_campaign, delete_campaign, get_campaign, list_campaigns, schedule_campaign, send_now_campaign, test_send_campaign, update_campaign};
use super::constants::{EVENT_NEWSLETTER_CAMPAIGN_SEND};
use super::lists::{create_list, delete_list, get_list, list_lists, update_list};
use super::sending::{recover_stale_sending_campaigns, spawn_campaign_worker};
use super::state::{NewsletterState};
use super::subscribers::{confirm_subscriber, confirm_subscriber_post, delete_subscriber, get_subscriber, list_list_subscribers, resend_confirm, subscribe, unsubscribe_subscriber, unsubscribe_via_link, update_subscriber};
use super::tracking::{track_click, track_open};
use super::webhooks::{overall_stats, provider_webhook};
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct NewsletterModule {
    pub(crate) state: Arc<NewsletterState>,
}

impl NewsletterModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(NewsletterState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NewsletterModule::new()));
}

impl Module for NewsletterModule {
    fn id(&self) -> &'static str {
        "newsletter"
    }

    fn name(&self) -> &'static str {
        "Newsletter"
    }

    fn description(&self) -> &'static str {
        "Newsletter lists, double opt-in subscribers, and campaigns built from GrapeJS email templates, sent through the Email Providers module with open/click tracking and provider webhooks."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["email_providers", "email-templates", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("newsletter", AI_CAPABILITIES);
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        // A crash mid-send leaves campaigns stuck in `sending`;
        // reset those to `failed` on startup so they can be
        // re-created instead of blocking their slot forever.
        recover_stale_sending_campaigns();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Execution entry point for scheduled campaigns (called back
        // by the scheduler, or directly by an operator).
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_CAMPAIGN_SEND,
            "newsletter",
            Arc::new(move |ctx: &mut HookContext| {
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                spawn_campaign_worker(state.clone(), campaign_id.clone());
                ctx.add_result(json!({ "queued": true, "campaign_id": campaign_id }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists", owner: "newsletter", description: "Paginated newsletter lists.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/lists", owner: "newsletter", description: "Create a newsletter list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "One newsletter list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "Update a newsletter list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "Delete a newsletter list and its subscribers and campaigns.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists/{id}/subscribers", owner: "newsletter", description: "Paginated subscribers of one list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/lists/{id}/subscribe", owner: "newsletter", description: "Subscribe an email address (double opt-in when the list requires it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "One subscriber.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "Update a subscriber.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "Delete a subscriber.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/subscribers/confirm", owner: "newsletter", description: "Confirm a double opt-in token.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/confirm", owner: "newsletter", description: "Confirm a double opt-in token.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/{id}/resend-confirm", owner: "newsletter", description: "Resend the confirmation email.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/{id}/unsubscribe", owner: "newsletter", description: "Unsubscribe a subscriber.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/unsubscribe", owner: "newsletter", description: "Unsubscribe through a signed link.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns", owner: "newsletter", description: "Paginated campaigns.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns", owner: "newsletter", description: "Create a campaign from a GrapeJS template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "One campaign.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "Update a draft or scheduled campaign.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "Delete a campaign.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/schedule", owner: "newsletter", description: "Schedule a campaign through the scheduler.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/send-now", owner: "newsletter", description: "Send a campaign immediately.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/test-send", owner: "newsletter", description: "Send a test copy of a campaign to one address.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns/{id}/stats", owner: "newsletter", description: "Campaign stats with rates.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/track/open/{campaign_id}/{subscriber_id}", owner: "newsletter", description: "Open-tracking pixel.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/track/click/{campaign_id}/{subscriber_id}", owner: "newsletter", description: "Click-tracking redirect.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/newsletter/webhooks/{provider}", owner: "newsletter", description: "Provider delivery/bounce/complaint webhook receiver.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/newsletter/stats", owner: "newsletter", description: "Overall newsletter stats.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/newsletter")
                .app_data(web::Data::new(state))
                .route("/stats", web::get().to(overall_stats))
                .route("/lists", web::get().to(list_lists))
                .route("/lists", web::post().to(create_list))
                .route("/lists/{id}", web::get().to(get_list))
                .route("/lists/{id}", web::put().to(update_list))
                .route("/lists/{id}", web::delete().to(delete_list))
                .route("/lists/{id}/subscribers", web::get().to(list_list_subscribers))
                .route("/lists/{id}/subscribe", web::post().to(subscribe))
                // Static segments first so `/subscribers/confirm` and
                // `/unsubscribe` are never captured by `/{id}` routes.
                .route("/subscribers/confirm", web::get().to(confirm_subscriber))
                .route("/subscribers/confirm", web::post().to(confirm_subscriber_post))
                .route("/unsubscribe", web::get().to(unsubscribe_via_link))
                .route("/subscribers/{id}", web::get().to(get_subscriber))
                .route("/subscribers/{id}", web::put().to(update_subscriber))
                .route("/subscribers/{id}", web::delete().to(delete_subscriber))
                .route("/subscribers/{id}/resend-confirm", web::post().to(resend_confirm))
                .route("/subscribers/{id}/unsubscribe", web::post().to(unsubscribe_subscriber))
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{id}", web::get().to(get_campaign))
                .route("/campaigns/{id}", web::put().to(update_campaign))
                .route("/campaigns/{id}", web::delete().to(delete_campaign))
                .route("/campaigns/{id}/schedule", web::post().to(schedule_campaign))
                .route("/campaigns/{id}/send-now", web::post().to(send_now_campaign))
                .route("/campaigns/{id}/test-send", web::post().to(test_send_campaign))
                .route("/campaigns/{id}/stats", web::get().to(campaign_stats))
                .route("/track/open/{campaign_id}/{subscriber_id}", web::get().to(track_open))
                .route("/track/click/{campaign_id}/{subscriber_id}", web::get().to(track_click))
                .route("/webhooks/{provider}", web::post().to(provider_webhook)),
        );
    }
}
