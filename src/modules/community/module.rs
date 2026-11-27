//! Module registration: routes, hooks and metadata.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::types::GAMIFICATION_BADGE_AWARDED;
use super::state::CommunityState;
use super::hooks::on_badge_awarded;
use super::handlers_categories::{create_category, create_topic, delete_category, delete_topic, get_category, get_topic, list_categories, list_topics, lock_topic, pin_topic, update_category, update_topic};
use super::handlers_posts::{create_post, delete_post, flag_post, get_post, get_post_votes, hide_post, list_flags, list_moderation_actions, list_topic_posts, review_flag, unhide_post, update_post, vote_post};
use super::handlers_members::{create_announcement, delete_announcement, get_announcement, get_member, get_public_topic, get_settings, join_community, list_announcements, list_members, list_public_topics, publish_announcement, update_announcement, update_settings};
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct CommunityModule {
    pub(crate) state: CommunityState,
}

impl CommunityModule {
    pub fn new() -> Self {
        Self {
            state: CommunityState::default(),
        }
    }
}

impl Default for CommunityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CommunityModule {
    fn id(&self) -> &'static str {
        "community"
    }

    fn name(&self) -> &'static str {
        "Community / Forum"
    }

    fn description(&self) -> &'static str {
        "Community forum with categories (public, members-only, staff-only), topics with pin and lock, posts with one-level nested replies and kept edit history, one vote per user per post, flag-based moderation with an audited action log and spam auto-hide, a member directory, announcements that notify members, and configurable rate limits. Badges are owned by the gamification module; this module only tracks badge counts."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "gamification"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "gamification"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("community", AI_CAPABILITIES);
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
            GAMIFICATION_BADGE_AWARDED,
            "community",
            Arc::new(move |ctx: &mut HookContext| {
                on_badge_awarded(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/community")
                .app_data(state)
                .route("/public/topics", web::get().to(list_public_topics))
                .route("/public/topics/{id}", web::get().to(get_public_topic))
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/topics", web::get().to(list_topics))
                .route("/topics", web::post().to(create_topic))
                .route("/topics/{id}", web::get().to(get_topic))
                .route("/topics/{id}", web::patch().to(update_topic))
                .route("/topics/{id}", web::delete().to(delete_topic))
                .route("/topics/{id}/pin", web::post().to(pin_topic))
                .route("/topics/{id}/lock", web::post().to(lock_topic))
                .route("/topics/{id}/posts", web::get().to(list_topic_posts))
                .route("/topics/{id}/posts", web::post().to(create_post))
                .route("/posts/{id}", web::get().to(get_post))
                .route("/posts/{id}", web::patch().to(update_post))
                .route("/posts/{id}", web::delete().to(delete_post))
                .route("/posts/{id}/vote", web::post().to(vote_post))
                .route("/posts/{id}/votes", web::get().to(get_post_votes))
                .route("/posts/{id}/flag", web::post().to(flag_post))
                .route("/posts/{id}/hide", web::post().to(hide_post))
                .route("/posts/{id}/unhide", web::post().to(unhide_post))
                .route("/flags", web::get().to(list_flags))
                .route("/flags/{id}/review", web::post().to(review_flag))
                .route("/moderation-actions", web::get().to(list_moderation_actions))
                .route("/members", web::get().to(list_members))
                .route("/members", web::post().to(join_community))
                .route("/members/{user_id}", web::get().to(get_member))
                .route("/announcements", web::get().to(list_announcements))
                .route("/announcements", web::post().to(create_announcement))
                .route("/announcements/{id}", web::get().to(get_announcement))
                .route("/announcements/{id}", web::patch().to(update_announcement))
                .route("/announcements/{id}", web::delete().to(delete_announcement))
                .route(
                    "/announcements/{id}/publish",
                    web::post().to(publish_announcement),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/community/categories", owner: "community", description: "List categories (paginated, filter by visibility; staff-only categories hidden without the manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/categories", owner: "community", description: "Create a category (order, visibility public/members-only/staff-only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/categories/{id}", owner: "community", description: "Get one category.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/community/categories/{id}", owner: "community", description: "Update a category.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/community/categories/{id}", owner: "community", description: "Delete a category with no topics.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/topics", owner: "community", description: "List topics (paginated, filter by category_id, tag, pinned, author_id; pinned first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/topics", owner: "community", description: "Create a topic (rate-limited per user per hour).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/topics/{id}", owner: "community", description: "Get one topic with its visible post count.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/community/topics/{id}", owner: "community", description: "Update a topic (author or moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/community/topics/{id}", owner: "community", description: "Delete a topic with its posts, votes, and flags (author or moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/pin", owner: "community", description: "Toggle the pinned flag of a topic (moderator, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/lock", owner: "community", description: "Toggle the locked flag of a topic (moderator, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/topics/{id}/posts", owner: "community", description: "List posts of a topic (paginated, filter by author_id, parent_post_id; hidden posts only for moderators and the author).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/posts", owner: "community", description: "Create a post or a one-level reply (parent_post_id must be a top-level post; rate-limited; locked topics reject posts).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/posts/{id}", owner: "community", description: "Get one post with vote totals and edit history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/community/posts/{id}", owner: "community", description: "Edit a post (author or moderator); the previous body is kept in the edit history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/community/posts/{id}", owner: "community", description: "Delete a post with its replies, votes, and flags (author or moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/vote", owner: "community", description: "Vote up (+1) or down (-1) on a post; one vote per user, changing it updates the row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/posts/{id}/votes", owner: "community", description: "Vote totals of a post plus the caller's own vote.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/flag", owner: "community", description: "Flag a post with a reason; reaching the spam threshold auto-hides it pending review.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/hide", owner: "community", description: "Hide a post (moderator, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/unhide", owner: "community", description: "Unhide a post (moderator, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/flags", owner: "community", description: "List flags, open first (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/flags/{id}/review", owner: "community", description: "Review a flag: dismiss it or hide the post (moderator, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/moderation-actions", owner: "community", description: "Audited moderator action log (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/members", owner: "community", description: "Member directory: display name, join date, badge counts, activity counts (never emails or private data).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/members", owner: "community", description: "Join the community (one row per user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/members/{user_id}", owner: "community", description: "Get one member from the directory.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/announcements", owner: "community", description: "List announcements (drafts only for moderators; pinned first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/announcements", owner: "community", description: "Create a draft announcement (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Get one announcement.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Update a draft announcement (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Delete an announcement (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/community/announcements/{id}/publish", owner: "community", description: "Publish an announcement and notify every member (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/settings", owner: "community", description: "Get rate limits and the spam flag threshold (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/community/settings", owner: "community", description: "Update rate limits and the spam flag threshold (moderator).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/community/public/topics", owner: "community", description: "PUBLIC: topics in public categories (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/community/public/topics/{id}", owner: "community", description: "PUBLIC: one public topic with its visible posts (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CommunityModule::new()));
}
