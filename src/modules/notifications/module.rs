// notifications module -- Module definition, registration, and route table.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct NotificationsModule {
    pub(crate) state: Arc<NotificationsState>,
}

impl NotificationsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(NotificationsState::new()),
        }
    }
}

impl Default for NotificationsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for NotificationsModule {
    fn id(&self) -> &'static str {
        "notifications"
    }

    fn name(&self) -> &'static str {
        "Notifications"
    }

    fn description(&self) -> &'static str {
        "Notifications with read tracking, per-user channel, category and digest preferences (including off), per-event notification templates with channel selection (email via an email-provider connection and GrapeJS email templates, in-app, webhook), delivery tracking with retries, audience targeting (per-user, role, department, segment, broadcast) enforced server-side, and listeners for the other modules' important events (orders, payments, backups, clusters, tickets, licenses, incidents, webhooks, security)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "email_providers"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("notifications", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_notification_requested(state.clone(), ctx));
        bus.register("notification.requested", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_security_auth_failed(state.clone(), ctx));
        bus.register("security.auth.failed", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_cluster_sync_failed(state.clone(), ctx));
        bus.register("cluster.sync.failed", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_payment_failed(state.clone(), ctx));
        bus.register("payment.failed", "notifications", handler);
        // Direct listeners for the other modules' important events.
        // See the "Domain-event listeners" section above for the
        // coverage table and the events deliberately NOT subscribed
        // here (their emitters already forward via
        // `notification.requested`).
        for event in [
            "order.paid",
            "invoice.paid",
            "payment.captured",
            "payment.refunded",
            "store.order.completed",
            "store.order.refunded",
            "store.order.cancelled",
            "ticket.commented",
            "ticket.assigned",
            "backup.failed",
            "backup.completed",
            "backup.restored",
            "cluster.health.changed",
            "cluster.node.unhealthy",
            "lb.backend.unhealthy",
            "cluster.sync.completed",
            "cluster.sync.conflict",
            "license.issued",
            "license.activated",
            "license.deactivated",
            "license.suspended",
            "license.revoked",
            "statuspage.incident.created",
            "statuspage.incident.resolved",
            "vcs.sync.failed",
            "webhook.failed",
            "security.bruteforce.locked",
            "security.ip.flagged",
        ] {
            bus.register(event, "notifications", domain_handler(self.state.clone(), event));
        }
        // Provider outcomes update the recorded delivery attempts
        // (matched by the job id `send_email` recorded when queuing).
        for (event, delivered) in [
            ("email.sent", true),
            ("email.failed", false),
            ("mail.sent", true),
            ("mail.failed", false),
        ] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_email_delivery(state.clone(), delivered, ctx)
            });
            bus.register(event, "notifications", handler);
        }
        // Webhook outcomes resolve the `queued` webhook attempts
        // `dispatch_channels` records (matched by notification id;
        // see `on_webhook_delivery`). Without these, a webhook
        // attempt stayed `queued` forever even after the webhooks
        // module delivered it or reported the failure.
        for (event, delivered) in [
            ("webhook.delivered", true),
            ("webhook.failed", false),
        ] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_webhook_delivery(state.clone(), delivered, ctx)
            });
            bus.register(event, "notifications", handler);
        }
        // Digest delivery entry point for the scheduler (or any other
        // module): `{ "user_id", "email"? }`. The digest itself is
        // sent through the Email Providers module, falling back to the
        // Mail Manager.
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            let user_id = ctx
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let email = ctx
                .get("email")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // A user who set the digest to `off` never gets a
            // scheduler-driven digest. The manual POST /digest/send
            // endpoint is an explicit operator action and bypasses
            // this guard.
            if state.preferences_for(&user_id).digest == "off" {
                ctx.add_result(json!({
                    "user_id": user_id,
                    "sent": false,
                    "reason": "digest is disabled for this user",
                }));
                return;
            }
            match send_digest_for_user(&state, &user_id, email.as_deref()) {
                Ok(result) => ctx.add_result(result),
                Err(error) => ctx.add_result(json!({ "error": error.message })),
            }
        });
        bus.register("notification.digest.send", "notifications", handler);
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/notifications", owner: "notifications", description: "List notifications", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications", owner: "notifications", description: "Create notification", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/preferences", owner: "notifications", description: "Get preferences", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/notifications/preferences", owner: "notifications", description: "Update preferences", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/unread-count", owner: "notifications", description: "Unread count", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/read-all", owner: "notifications", description: "Mark all read", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/digest/send", owner: "notifications", description: "Send digest", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/audiences", owner: "notifications", description: "List registered audiences (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/audiences", owner: "notifications", description: "Register an audience (role/department/segment members).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/audiences/{audience}", owner: "notifications", description: "Get one registered audience.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/audiences/{audience}", owner: "notifications", description: "Delete a registered audience.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/deliveries", owner: "notifications", description: "List delivery attempts (paginated, filter by notification_id, channel, status, job_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/{id}/deliveries", owner: "notifications", description: "List delivery attempts for one notification (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/deliveries/{attempt_id}/retry", owner: "notifications", description: "Retry one failed delivery attempt (max 3 attempts).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/templates", owner: "notifications", description: "List templates", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates", owner: "notifications", description: "Create template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Get template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Update template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Delete template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates/{id}/preview", owner: "notifications", description: "Preview template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates/{id}/test", owner: "notifications", description: "Test template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/notifications/{id}", owner: "notifications", description: "Get notification", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/{id}", owner: "notifications", description: "Delete notification", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/read", owner: "notifications", description: "Mark read", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/unread", owner: "notifications", description: "Mark unread", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/notifications")
                .route("", web::get().to(list_notifications))
                .route("", web::post().to(create_notification))
                .route("/preferences", web::get().to(get_preferences))
                .route("/preferences", web::put().to(update_preferences))
                .route("/unread-count", web::get().to(unread_count))
                .route("/read-all", web::post().to(mark_all_read))
                .route("/digest/send", web::post().to(send_digest))
                .route("/audiences", web::get().to(list_audiences))
                .route("/audiences", web::post().to(create_audience))
                .route("/audiences/{audience}", web::get().to(get_audience))
                .route("/audiences/{audience}", web::delete().to(delete_audience))
                .route("/deliveries", web::get().to(list_deliveries))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::put().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/templates/{id}/preview", web::post().to(preview_template))
                .route("/templates/{id}/test", web::post().to(test_template))
                .route("/{id}", web::get().to(get_notification))
                .route("/{id}", web::delete().to(delete_notification))
                .route("/{id}/read", web::post().to(mark_read))
                .route("/{id}/unread", web::post().to(mark_unread))
                .route("/{id}/deliveries", web::get().to(list_notification_deliveries))
                .route("/{id}/deliveries/{attempt_id}/retry", web::post().to(retry_delivery)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NotificationsModule::new()));
}
