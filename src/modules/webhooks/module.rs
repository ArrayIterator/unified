// Webhooks — Module definition.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct WebhooksModule;

impl Module for WebhooksModule {
    fn id(&self) -> &'static str {
        "webhooks"
    }

    fn name(&self) -> &'static str {
        "Webhooks Manager"
    }

    fn description(&self) -> &'static str {
        "Outgoing webhooks: endpoint subscriptions with HMAC-SHA256 signed deliveries, retry with exponential backoff, a per-attempt delivery log, manual redelivery, and test deliveries."
    }

    fn requires(&self) -> &[&'static str] {
        &["security"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2", "url"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // One handler per subscribable event. The handler only fans
        // the event out to matching endpoints; the HTTP work happens
        // in background tasks, never on the emitting thread.
        // Webhook lifecycle events (`webhook.*`) are deliberately NOT
        // re-dispatched: a delivery emits `webhook.delivered`, and
        // fanning that out again would loop forever through any
        // endpoint subscribed to it. `webhook.ping` is only ever sent
        // by an explicit test request.
        for event_name in KNOWN_EVENTS {
            if event_name.starts_with("webhook.") || *event_name == "notification.webhook" {
                continue;
            }
            let event = event_name.to_string();
            let handler: unified_core::hooks::HookHandler =
                Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                    let payload = context.data.clone();
                    dispatch_event(&event, &payload);
                });
            bus.register(&event, "webhooks", handler);
        }
        // The notifications module emits `notification.webhook` when
        // the webhook channel is selected for a notification, with
        // `{"event", "notification_id", "user_id", "title", "body",
        // "kind"}` (plus `"retry": true` on a retry). This listener
        // delivers that payload to every active, unpaused endpoint
        // subscribed to `notification.webhook`, through the same
        // `dispatch_event` / delivery-record path as every other
        // event, and reports the outcome as the hook result. When no
        // endpoint matches, the miss is recorded as a
        // `webhook.failed` event (which the notifications module
        // subscribes to) instead of being silently dropped: a
        // notification whose webhook channel delivers nowhere is a
        // misconfiguration the operator must see.
        let handler: unified_core::hooks::HookHandler =
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let payload = context.data.clone();
                let deliveries = dispatch_event("notification.webhook", &payload);
                context.add_result(serde_json::json!({
                    "matched_endpoints": deliveries.len(),
                    "delivery_ids": deliveries,
                }));
                if deliveries.is_empty() {
                    emit(
                        webhook_events::FAILED,
                        serde_json::json!({
                            "event": "notification.webhook",
                            "endpoint_id": "none",
                            "notification_id": payload.get("notification_id"),
                            "error": "No webhook endpoint is subscribed to \"notification.webhook\", so the notification was not delivered anywhere",
                            "attempts": 0,
                        }),
                    );
                }
            });
        bus.register("notification.webhook", "webhooks", handler);
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/webhooks")
                .route("/events", web::get().to(list_events))
                .route("/endpoints", web::get().to(list_endpoints))
                .route("/endpoints", web::post().to(create_endpoint))
                .route("/endpoints/{id}", web::get().to(get_endpoint))
                .route("/endpoints/{id}", web::put().to(update_endpoint))
                .route("/endpoints/{id}", web::delete().to(delete_endpoint))
                .route("/endpoints/{id}/pause", web::post().to(pause_endpoint))
                .route("/endpoints/{id}/resume", web::post().to(resume_endpoint))
                .route("/endpoints/{id}/test", web::post().to(test_endpoint))
                .route("/deliveries", web::get().to(list_deliveries))
                .route("/deliveries/{id}", web::get().to(get_delivery))
                .route("/deliveries/{id}/redeliver", web::post().to(redeliver)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/webhooks/events", owner: "webhooks", description: "Catalog of subscribable event names", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/webhooks/endpoints", owner: "webhooks", description: "List webhook endpoints (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints", owner: "webhooks", description: "Create a webhook endpoint subscription", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Get one webhook endpoint", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Update a webhook endpoint", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Delete a webhook endpoint", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/pause", owner: "webhooks", description: "Pause deliveries to an endpoint", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/resume", owner: "webhooks", description: "Resume deliveries to an endpoint", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/test", owner: "webhooks", description: "Send a webhook.ping test delivery", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/webhooks/deliveries", owner: "webhooks", description: "List webhook deliveries (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/webhooks/deliveries/{id}", owner: "webhooks", description: "Get one webhook delivery", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/webhooks/deliveries/{id}/redeliver", owner: "webhooks", description: "Redeliver a webhook delivery", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WebhooksModule));
}
