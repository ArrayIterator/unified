//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_accounts::{
    connect_account, create_account, delete_account, disconnect_account, get_account,
    list_accounts, rotate_account_webhook_secret, test_account, update_account,
};
use super::handlers_ai::ai_capability;
use super::handlers_audit::list_audit;
use super::handlers_campaigns::{
    campaign_rollup, create_campaign, delete_campaign, get_campaign, list_campaign_posts,
    list_campaigns, update_campaign,
};
use super::handlers_metrics::{account_rollup, list_metrics, metrics_ingest, record_metric};
use super::handlers_networks::{
    create_network, delete_network, get_network, list_networks, update_network,
};
use super::handlers_posts::{
    cancel_post, create_post, delete_post, get_post, list_posts, publish_post_now, retry_post,
    schedule_post, update_post,
};
use super::hooks::{
    ai_readiness_event, ai_summarize_event, on_ai_readiness_requested, on_ai_summarize_requested,
    on_marketing_campaign_created, on_marketing_campaign_sent, on_user_deleted,
};
use super::state::SocialMarketingState;
use super::types::{
    EVENT_MARKETING_CAMPAIGN_CREATED, EVENT_MARKETING_CAMPAIGN_SENT, EVENT_USER_DELETED,
};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// Module

pub struct SocialMarketingModule {
    pub(crate) state: SocialMarketingState,
}

impl SocialMarketingModule {
    pub fn new() -> Self {
        Self {
            state: SocialMarketingState::default(),
        }
    }
}

impl Default for SocialMarketingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SocialMarketingModule {
    fn id(&self) -> &'static str {
        "social_marketing"
    }

    fn name(&self) -> &'static str {
        "Social Marketing"
    }

    fn description(&self) -> &'static str {
        "Social marketing: admin-defined networks as data (API base, auth type, publish path, supported post types, text and media limits, rate limit), connected accounts with write-only encrypted credentials, token expiry, and a metrics-ingest webhook secret, scheduled posts with a draft / scheduled / publishing / published / failed / cancelled status machine publishing once per target account through the generic HTTP adapter or the log-only test adapter with one stored result per account, campaigns grouping posts with a goal as data and live post-count and metrics rollups, and analytics metrics that arrive only through the provider verification-token webhook or manual entry (never live-fetched), with an append-only audit trail."
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
        Some("marketing")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("social_marketing", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "social_marketing",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CAMPAIGN_CREATED,
            "social_marketing",
            Arc::new(move |ctx: &mut HookContext| {
                on_marketing_campaign_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CAMPAIGN_SENT,
            "social_marketing",
            Arc::new(move |ctx: &mut HookContext| on_marketing_campaign_sent(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "social_marketing",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_readiness_event(),
            "social_marketing",
            Arc::new(move |ctx: &mut HookContext| on_ai_readiness_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/social-marketing")
                .app_data(state)
                .route("/networks", web::get().to(list_networks))
                .route("/networks", web::post().to(create_network))
                .route("/networks/{network_id}", web::get().to(get_network))
                .route("/networks/{network_id}", web::patch().to(update_network))
                .route("/networks/{network_id}", web::delete().to(delete_network))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/{account_id}", web::get().to(get_account))
                .route("/accounts/{account_id}", web::patch().to(update_account))
                .route("/accounts/{account_id}", web::delete().to(delete_account))
                .route(
                    "/accounts/{account_id}/connect",
                    web::post().to(connect_account),
                )
                .route(
                    "/accounts/{account_id}/disconnect",
                    web::post().to(disconnect_account),
                )
                .route(
                    "/accounts/{account_id}/rotate-webhook-secret",
                    web::post().to(rotate_account_webhook_secret),
                )
                .route("/accounts/{account_id}/test", web::post().to(test_account))
                .route(
                    "/accounts/{account_id}/rollup",
                    web::get().to(account_rollup),
                )
                .route("/posts", web::get().to(list_posts))
                .route("/posts", web::post().to(create_post))
                .route("/posts/{post_id}", web::get().to(get_post))
                .route("/posts/{post_id}", web::patch().to(update_post))
                .route("/posts/{post_id}", web::delete().to(delete_post))
                .route("/posts/{post_id}/schedule", web::post().to(schedule_post))
                .route(
                    "/posts/{post_id}/publish-now",
                    web::post().to(publish_post_now),
                )
                .route("/posts/{post_id}/cancel", web::post().to(cancel_post))
                .route("/posts/{post_id}/retry", web::post().to(retry_post))
                .route("/posts/{post_id}/metrics", web::post().to(record_metric))
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{campaign_id}", web::get().to(get_campaign))
                .route("/campaigns/{campaign_id}", web::patch().to(update_campaign))
                .route(
                    "/campaigns/{campaign_id}",
                    web::delete().to(delete_campaign),
                )
                .route(
                    "/campaigns/{campaign_id}/posts",
                    web::get().to(list_campaign_posts),
                )
                .route(
                    "/campaigns/{campaign_id}/rollup",
                    web::get().to(campaign_rollup),
                )
                .route("/metrics", web::get().to(list_metrics))
                .route(
                    "/metrics-ingest/{account_id}",
                    web::post().to(metrics_ingest),
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
                path: "/api/v1/social-marketing/networks",
                owner: "social_marketing",
                description: "List social networks (paginated, filter by kind, auth type, enabled; search by name/slug; filter by created date range).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/networks",
                owner: "social_marketing",
                description: "Create a social network with slug, kind, HTTPS API base, publish path, auth type, supported post types, text and media limits, and per-network rate limit (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/networks/{network_id}",
                owner: "social_marketing",
                description: "Get one social network.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/social-marketing/networks/{network_id}",
                owner: "social_marketing",
                description: "Update a social network (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/social-marketing/networks/{network_id}",
                owner: "social_marketing",
                description: "Delete a social network; refused while any account is connected to it (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/accounts",
                owner: "social_marketing",
                description: "List connected social accounts (paginated, filter by network, connected, enabled, token-expired; search by handle/display name; masked view - no credential).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/accounts",
                owner: "social_marketing",
                description: "Create a social account row (starts disconnected; the metrics-ingest webhook secret is generated and returned once) (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/accounts/{account_id}",
                owner: "social_marketing",
                description: "Get one social account (masked view - the credential and webhook secret never appear).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/social-marketing/accounts/{account_id}",
                owner: "social_marketing",
                description: "Update a social account's handle, display name, token expiry, or enabled flag (manager; the credential changes only through connect).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/social-marketing/accounts/{account_id}",
                owner: "social_marketing",
                description: "Delete a social account; refused while any post targets it (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/accounts/{account_id}/connect",
                owner: "social_marketing",
                description: "Connect an account: store the credential write-only encrypted, mark it connected, and return a fresh webhook secret once when one is supplied or none is stored (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/accounts/{account_id}/disconnect",
                owner: "social_marketing",
                description: "Disconnect an account: mark it disconnected and clear the stored credential (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/accounts/{account_id}/rotate-webhook-secret",
                owner: "social_marketing",
                description: "Rotate an account's metrics-ingest webhook secret; the fresh raw secret is returned once (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/accounts/{account_id}/test",
                owner: "social_marketing",
                description: "Dry-run check of an account's configuration (network, connection, token expiry, stored credential readability). Makes no network call and publishes nothing (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/accounts/{account_id}/rollup",
                owner: "social_marketing",
                description: "Per-account metrics rollup: summed metrics of every post targeting the account, post count, and engagement rate in basis points.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/posts",
                owner: "social_marketing",
                description: "List posts (paginated, filter by network, account, status, campaign, post type; search by text/hashtags; filter by created date range).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts",
                owner: "social_marketing",
                description: "Create a post with network, target accounts, post type, text, media references by id, link, and hashtags; a future send_at starts it as scheduled (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/posts/{post_id}",
                owner: "social_marketing",
                description: "Get one post with its per-account publish results and its summed metrics.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/social-marketing/posts/{post_id}",
                owner: "social_marketing",
                description: "Update a post; only drafts and cancelled posts are editable (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/social-marketing/posts/{post_id}",
                owner: "social_marketing",
                description: "Delete a post; only drafts and cancelled posts can be deleted, its metric rows are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts/{post_id}/schedule",
                owner: "social_marketing",
                description: "Schedule a post for a future publish time; emits social.post.scheduled for a scheduler to pick up - this module runs no runner (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts/{post_id}/publish-now",
                owner: "social_marketing",
                description: "Publish a post now: dispatches once per target account through the network's provider adapter, enforces the per-network per-minute limit, and stores one result per account (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts/{post_id}/cancel",
                owner: "social_marketing",
                description: "Cancel a draft, scheduled, or failed post (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts/{post_id}/retry",
                owner: "social_marketing",
                description: "Retry a failed post through the same publish pass as publish-now (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/posts/{post_id}/metrics",
                owner: "social_marketing",
                description: "Record one metric value for a post by hand; re-recording the same metric replaces the value (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/campaigns",
                owner: "social_marketing",
                description: "List campaigns (paginated, filter by status, goal metric; search by name/goal; filter by created date range).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/campaigns",
                owner: "social_marketing",
                description: "Create a campaign with name, date range, and a goal as data (goal label, goal metric slug, integer goal target) (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/campaigns/{campaign_id}",
                owner: "social_marketing",
                description: "Get one campaign with its live post counts, summed metrics, engagement rate, and goal progress.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/social-marketing/campaigns/{campaign_id}",
                owner: "social_marketing",
                description: "Update a campaign; fields edit only in draft or cancelled, the status moves through its machine (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/social-marketing/campaigns/{campaign_id}",
                owner: "social_marketing",
                description: "Delete a campaign; only drafts and cancelled campaigns can be deleted, its posts keep their campaign id (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/campaigns/{campaign_id}/posts",
                owner: "social_marketing",
                description: "List the posts of one campaign (paginated, filter by status, network, account; search by text/hashtags).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/campaigns/{campaign_id}/rollup",
                owner: "social_marketing",
                description: "Per-campaign metrics rollup: summed metrics of every post in the campaign, post counts by status, engagement rate, and goal progress in basis points.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/metrics",
                owner: "social_marketing",
                description: "List metric rows (paginated, filter by post, account, metric, source; filter by recorded date range).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/metrics-ingest/{account_id}",
                owner: "social_marketing",
                description: "PUBLIC (webhook-secret gated): provider metrics-ingest callback - upserts reported metric values for one post targeting this account. Metrics are reported values only; this module never fetches live numbers.",
                auth: public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/social-marketing/audit",
                owner: "social_marketing",
                description: "List the audit trail (paginated, filter by entity type, entity, action, actor; newest first).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/social-marketing/ai/{capability}",
                owner: "social_marketing",
                description: "Run one registered social marketing AI capability through the AI module's gated runner.",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SocialMarketingModule::new()));
}
