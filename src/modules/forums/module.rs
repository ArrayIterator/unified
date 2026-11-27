//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_forums::{
    create_forum, create_forum_category, delete_category, delete_forum, get_category, get_forum,
    get_public_forum, get_public_thread, list_forum_categories, list_forums,
    list_public_forum_threads, list_public_forums, public_search, update_category,
    update_forum,
};
use super::handlers_moderation::{
    get_reputation, get_settings, list_moderation_actions, list_reports,
    list_reputation_leaderboard, review_report, update_settings,
};
use super::handlers_polls::{close_poll, create_poll, get_poll, get_thread_poll, vote_poll};
use super::handlers_posts::{
    create_post, delete_post, get_post, get_post_votes, list_thread_posts, report_post,
    update_post, vote_post,
};
use super::handlers_subscriptions::{
    list_subscriptions, subscribe_forum, subscribe_thread, unsubscribe_forum, unsubscribe_thread,
};
use super::handlers_threads::{
    create_thread, delete_thread, get_thread, list_forum_threads, lock_thread, pin_thread,
    report_thread, search_forums, solve_thread, update_thread, view_thread,
};
use super::hooks::{on_badge_awarded, on_search_reindex_requested, on_user_deleted};
use super::state::ForumState;
use super::types::{
    EVENT_GAMIFICATION_BADGE_AWARDED, EVENT_SEARCH_REINDEX_REQUESTED, EVENT_USER_DELETED,
};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// Module

pub struct ForumsModule {
    pub(crate) state: ForumState,
}

impl ForumsModule {
    pub fn new() -> Self {
        Self {
            state: ForumState::default(),
        }
    }
}

impl Default for ForumsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ForumsModule {
    fn id(&self) -> &'static str {
        "forums"
    }

    fn name(&self) -> &'static str {
        "Forums"
    }

    fn description(&self) -> &'static str {
        "Public-facing discussion forums: SEO-friendly public forums with categories, threads with solved/best-answer marking, two-level nested replies with quotes and mentions, polls, thread/forum subscriptions with notifications, voting with reputation and a leaderboard, report-based moderation with an audited action log and new-user spam controls, and search indexing. Built on the same hook bus as the community module; it does not duplicate community's member directory or announcements."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "gamification", "search"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "gamification", "search"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("forums", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "forums",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_GAMIFICATION_BADGE_AWARDED,
            "forums",
            Arc::new(move |ctx: &mut HookContext| on_badge_awarded(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SEARCH_REINDEX_REQUESTED,
            "forums",
            Arc::new(move |ctx: &mut HookContext| on_search_reindex_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/forums")
                .app_data(state)
                .route("/public/forums", web::get().to(list_public_forums))
                .route("/public/forums/{slug}", web::get().to(get_public_forum))
                .route(
                    "/public/forums/{slug}/threads",
                    web::get().to(list_public_forum_threads),
                )
                .route("/public/threads/{id}", web::get().to(get_public_thread))
                .route("/public/search", web::get().to(public_search))
                .route("/forums", web::get().to(list_forums))
                .route("/forums", web::post().to(create_forum))
                .route("/forums/{id}", web::get().to(get_forum))
                .route("/forums/{id}", web::patch().to(update_forum))
                .route("/forums/{id}", web::delete().to(delete_forum))
                .route(
                    "/forums/{forum_id}/categories",
                    web::get().to(list_forum_categories),
                )
                .route(
                    "/forums/{forum_id}/categories",
                    web::post().to(create_forum_category),
                )
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route(
                    "/forums/{forum_id}/threads",
                    web::get().to(list_forum_threads),
                )
                .route("/forums/{forum_id}/threads", web::post().to(create_thread))
                .route("/threads/{id}", web::get().to(get_thread))
                .route("/threads/{id}", web::patch().to(update_thread))
                .route("/threads/{id}", web::delete().to(delete_thread))
                .route("/threads/{id}/pin", web::post().to(pin_thread))
                .route("/threads/{id}/lock", web::post().to(lock_thread))
                .route("/threads/{id}/solve", web::post().to(solve_thread))
                .route("/threads/{id}/view", web::post().to(view_thread))
                .route("/threads/{id}/report", web::post().to(report_thread))
                .route("/threads/{id}/subscribe", web::post().to(subscribe_thread))
                .route(
                    "/threads/{id}/unsubscribe",
                    web::post().to(unsubscribe_thread),
                )
                .route("/threads/{id}/poll", web::get().to(get_thread_poll))
                .route("/threads/{id}/poll", web::post().to(create_poll))
                .route("/threads/{id}/posts", web::get().to(list_thread_posts))
                .route("/threads/{id}/posts", web::post().to(create_post))
                .route("/posts/{id}", web::get().to(get_post))
                .route("/posts/{id}", web::patch().to(update_post))
                .route("/posts/{id}", web::delete().to(delete_post))
                .route("/posts/{id}/vote", web::post().to(vote_post))
                .route("/posts/{id}/votes", web::get().to(get_post_votes))
                .route("/posts/{id}/report", web::post().to(report_post))
                .route("/polls/{id}", web::get().to(get_poll))
                .route("/polls/{id}/vote", web::post().to(vote_poll))
                .route("/polls/{id}/close", web::post().to(close_poll))
                .route("/forums/{id}/subscribe", web::post().to(subscribe_forum))
                .route(
                    "/forums/{id}/unsubscribe",
                    web::post().to(unsubscribe_forum),
                )
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route("/reports", web::get().to(list_reports))
                .route("/reports/{id}/review", web::post().to(review_report))
                .route(
                    "/moderation-actions",
                    web::get().to(list_moderation_actions),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/search", web::get().to(search_forums))
                .route(
                    "/reputation/leaderboard",
                    web::get().to(list_reputation_leaderboard),
                )
                .route("/reputation/{user_id}", web::get().to(get_reputation)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/public/forums",
                owner: "forums",
                description: "PUBLIC: list public forums (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/public/forums/{slug}",
                owner: "forums",
                description: "PUBLIC: one public forum by slug (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/public/forums/{slug}/threads",
                owner: "forums",
                description: "PUBLIC: threads of a public forum (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/public/threads/{id}",
                owner: "forums",
                description: "PUBLIC: one public thread with its visible posts (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/public/search",
                owner: "forums",
                description: "PUBLIC: substring search over public threads and posts (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/forums",
                owner: "forums",
                description: "List forums (paginated; staff-only forums hidden without the manage capability).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/forums",
                owner: "forums",
                description: "Create a forum with an SEO slug (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/forums/{id}",
                owner: "forums",
                description: "Get one forum.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/forums/forums/{id}",
                owner: "forums",
                description: "Update a forum (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/forums/forums/{id}",
                owner: "forums",
                description: "Delete a forum with no categories or threads (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/forums/{forum_id}/categories",
                owner: "forums",
                description: "List categories of a forum (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/forums/{forum_id}/categories",
                owner: "forums",
                description: "Create a category inside a forum (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/categories/{id}",
                owner: "forums",
                description: "Get one category.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/forums/categories/{id}",
                owner: "forums",
                description: "Update a category (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/forums/categories/{id}",
                owner: "forums",
                description: "Delete a category with no threads (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/forums/{forum_id}/threads",
                owner: "forums",
                description: "List threads of a forum (paginated, filter by category_id, tag, pinned, solved, author_id; pinned first).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/forums/{forum_id}/threads",
                owner: "forums",
                description: "Create a thread with its opening post (rate-limited per user per hour).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/threads/{id}",
                owner: "forums",
                description: "Get one thread with its visible post count.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/forums/threads/{id}",
                owner: "forums",
                description: "Update a thread (author or moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/forums/threads/{id}",
                owner: "forums",
                description: "Delete a thread with its posts, votes, poll, and subscriptions (author or moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/pin",
                owner: "forums",
                description: "Toggle the pinned flag of a thread (moderator, audited).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/lock",
                owner: "forums",
                description: "Toggle the locked flag of a thread (moderator, audited).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/solve",
                owner: "forums",
                description: "Mark a thread solved with a best-answer post (author or moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/view",
                owner: "forums",
                description: "Record one view of a thread.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/report",
                owner: "forums",
                description: "Report a thread with a reason; the report is queued for moderator review.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/subscribe",
                owner: "forums",
                description: "Subscribe to a thread (idempotent; returns the existing subscription when already subscribed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/unsubscribe",
                owner: "forums",
                description: "Unsubscribe from a thread.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/threads/{id}/poll",
                owner: "forums",
                description: "Get the poll attached to a thread (404 when the thread has no poll).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/poll",
                owner: "forums",
                description: "Create a poll on a thread (author or moderator; 2-10 options; one poll per thread).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/threads/{id}/posts",
                owner: "forums",
                description: "List posts of a thread (paginated, filter by author_id, parent_post_id; hidden posts only for moderators and the author).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/threads/{id}/posts",
                owner: "forums",
                description: "Create a post or a nested reply (depth 0-2; rate-limited; locked threads reject posts; new-user link limits apply).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/posts/{id}",
                owner: "forums",
                description: "Get one post with vote totals and edit history.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/forums/posts/{id}",
                owner: "forums",
                description: "Edit a post (author or moderator); the previous body is kept in the edit history.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/forums/posts/{id}",
                owner: "forums",
                description: "Delete a post with its replies and votes (author or moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/posts/{id}/vote",
                owner: "forums",
                description: "Vote up (+1) or down (-1) on a post; one vote per user, changing it updates the row.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/posts/{id}/votes",
                owner: "forums",
                description: "Vote totals of a post plus the caller's own vote.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/posts/{id}/report",
                owner: "forums",
                description: "Report a post with a reason; reaching the spam threshold auto-hides it pending review.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/polls/{id}",
                owner: "forums",
                description: "Get one poll with its options, vote counts, and the caller's vote.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/polls/{id}/vote",
                owner: "forums",
                description: "Vote in a poll; one vote per user per poll.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/polls/{id}/close",
                owner: "forums",
                description: "Close a poll (poll creator or moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/forums/{id}/subscribe",
                owner: "forums",
                description: "Subscribe to a forum (idempotent; returns the existing subscription when already subscribed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/forums/{id}/unsubscribe",
                owner: "forums",
                description: "Unsubscribe from a forum.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/subscriptions",
                owner: "forums",
                description: "List the caller's own subscriptions (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/reports",
                owner: "forums",
                description: "List reports, open first (moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/forums/reports/{id}/review",
                owner: "forums",
                description: "Review a report: dismiss it, hide the target, or delete the target (moderator, audited).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/moderation-actions",
                owner: "forums",
                description: "Audited moderator action log (moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/settings",
                owner: "forums",
                description: "Get rate limits, the spam report threshold, and new-user restrictions (moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/forums/settings",
                owner: "forums",
                description: "Update rate limits, the spam report threshold, and new-user restrictions (moderator).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/search",
                owner: "forums",
                description: "Substring search over threads and posts in this tenant.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/reputation/leaderboard",
                owner: "forums",
                description: "Top users by reputation score in this tenant (?limit=, default 20, max 100).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/forums/reputation/{user_id}",
                owner: "forums",
                description: "Reputation totals of one user (recomputed from stored rows; 404 when the user has no forum activity).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ForumsModule::new()));
}
