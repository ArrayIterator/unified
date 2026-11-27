//! Module registration: routes, hooks and metadata.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use crate::modules::ai;
use actix_web::{web};
use serde_json::{json};
use std::sync::{Arc};
use super::state::AnnouncementsState;
use super::handlers_announcements::{archive_announcement, create_announcement, dashboard_feed, delete_announcement, get_announcement, list_announcement_reads, list_announcements, list_public_announcements, list_unread, mark_announcement_read, publish_announcement, publish_due, update_announcement};
use super::handlers_changelog::{create_changelog, delete_changelog, get_changelog, list_changelog, update_changelog};
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct AnnouncementsModule {
    pub(crate) state: Arc<AnnouncementsState>,
}

impl AnnouncementsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AnnouncementsState::new()),
        }
    }
}

impl Default for AnnouncementsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AnnouncementsModule {
    fn id(&self) -> &'static str {
        "announcements"
    }

    fn name(&self) -> &'static str {
        "Announcements and Changelog"
    }

    fn description(&self) -> &'static str {
        "Announcements with audience targeting (all, users, admins, role, department, segment, explicit users), channels (in-app, email, webhook), scheduling with publish-due, expiry, pinning, and read tracking, plus a public changelog and a user-dashboard feed."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("announcements", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Publish-due entry point for the scheduler (or any other
        // module): `{}`. The scheduler needs the
        // `announcement.publish_due` job kind to drive this on a
        // schedule (see the report); the endpoint
        // `POST /api/v1/announcements/publish-due` does the same
        // thing on demand.
        let state = self.state.clone();
        bus.register(
            "announcement.publish_due",
            "announcements",
            Arc::new(move |ctx: &mut HookContext| {
                let published = state.publish_due();
                ctx.add_result(json!({
                    "published": published.len(),
                    "announcement_ids": published
                        .iter()
                        .map(|announcement| announcement.id)
                        .collect::<Vec<_>>(),
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/announcements")
                .route("", web::get().to(list_announcements))
                .route("", web::post().to(create_announcement))
                // Static sub-paths must be registered before `{id}` so
                // Actix does not match them as an announcement id.
                .route("/public", web::get().to(list_public_announcements))
                .route("/unread", web::get().to(list_unread))
                .route("/dashboard", web::get().to(dashboard_feed))
                .route("/publish-due", web::post().to(publish_due))
                .route("/changelog", web::get().to(list_changelog))
                .route("/changelog", web::post().to(create_changelog))
                .route("/changelog/{id}", web::get().to(get_changelog))
                .route("/changelog/{id}", web::put().to(update_changelog))
                .route("/changelog/{id}", web::delete().to(delete_changelog))
                .route("/{id}", web::get().to(get_announcement))
                .route("/{id}", web::put().to(update_announcement))
                .route("/{id}", web::delete().to(delete_announcement))
                .route("/{id}/publish", web::post().to(publish_announcement))
                .route("/{id}/archive", web::post().to(archive_announcement))
                .route("/{id}/read", web::post().to(mark_announcement_read))
                .route("/{id}/reads", web::get().to(list_announcement_reads)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/announcements", owner: "announcements", description: "List announcements, all statuses (paginated; requires announcements.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/announcements", owner: "announcements", description: "Create an announcement draft (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/public", owner: "announcements", description: "PUBLIC: published, currently visible announcements, audience-filtered for the caller from the authenticated identity headers (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/announcements/unread", owner: "announcements", description: "Published announcements the authenticated caller has not read yet (caller identity from headers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/dashboard", owner: "announcements", description: "User-dashboard feed: audience-filtered announcements plus the unread count for the authenticated caller (caller identity from headers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/announcements/publish-due", owner: "announcements", description: "Publish every announcement whose publish time has arrived (also via the announcement.publish_due hook; requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Get one announcement (requires announcements.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Update an announcement (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Delete an announcement and its read receipts (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/publish", owner: "announcements", description: "Publish an announcement (fans out immediately when its publish time has arrived; otherwise stays scheduled until publish-due; requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/archive", owner: "announcements", description: "Archive an announcement (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/read", owner: "announcements", description: "Mark an announcement read for the authenticated caller (the body user_id must match the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/{id}/reads", owner: "announcements", description: "List read receipts for an announcement (paginated; requires announcements.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/changelog", owner: "announcements", description: "PUBLIC: changelog entries, newest first (paginated; no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/announcements/changelog", owner: "announcements", description: "Create a changelog entry (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "PUBLIC: get one changelog entry (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "PUT", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "Update a changelog entry (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "Delete a changelog entry (requires announcements.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AnnouncementsModule::new()));
}
