//! WhatsApp external module (split from lib.rs; moved verbatim).

use super::*;

// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

/// The WhatsApp Business connector module.
pub struct WhatsAppModule {
    state: Arc<WhatsAppState>,
}

impl WhatsAppModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(WhatsAppState::new(WhatsAppConfig::from_env())),
        }
    }

    /// Shared state, for tests and embedding.
    pub fn state(&self) -> Arc<WhatsAppState> {
        self.state.clone()
    }
}

impl Default for WhatsAppModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WhatsAppModule {
    fn id(&self) -> &'static str {
        "whatsapp"
    }

    fn name(&self) -> &'static str {
        "WhatsApp Business"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "WhatsApp Business Cloud API connector: outbound messaging, templates, webhook, message log, and conversations."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::APP_START,
            "whatsapp",
            Arc::new(move |ctx: &mut HookContext| {
                let config = state.config_snapshot();
                ctx.add_result(json!({
                "module": "whatsapp",
                "event": "started",
                "delivery_configured": config.is_delivery_configured(),
                "webhook_configured": config.verify_token.is_some() && config.app_secret.is_some(),
            }));
            }),
        );

        // Order-paid notification intent. `store.order.paid` is not a
        // core event (see ORDER_PAID_EVENT); this stays dormant until a
        // store module emits it. The handler only records an intent:
        // it never sends a message. An intent is `ready` only when the
        // tenant has a matching template AND credentials are
        // configured; otherwise it is `blocked` with a reason.
        let state = self.state.clone();
        bus.register(
            ORDER_PAID_EVENT,
            "whatsapp",
            Arc::new(move |ctx: &mut HookContext| {
                let tenant_id = ctx
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let phone = ctx
                    .get("phone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let order_id = ctx
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let template_name = ctx
                    .get("template_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("order_paid")
                    .to_string();
                let template_language = ctx
                    .get("template_language")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_TEMPLATE_LANGUAGE)
                    .to_string();
                if tenant_id.is_empty() || order_id.is_empty() || !is_valid_e164(&phone) {
                    ctx.add_result(json!({
                        "module": "whatsapp",
                        "event": ORDER_PAID_EVENT,
                        "intent": "blocked",
                        "reason": "order-paid payload is missing tenant_id/order_id or the buyer phone is not E.164",
                    }));
                    return;
                }
                let config = state.config_snapshot();
                let template_ready = state
                    .get_template(&tenant_id, &template_name, &template_language)
                    .is_some();
                let (status, reason) = if template_ready && config.is_delivery_configured() {
                    (
                        "ready",
                        "template and WhatsApp credentials are configured; send via POST /api/v1/whatsapp/send",
                    )
                } else if !template_ready {
                    (
                        "blocked",
                        "no matching WhatsApp template is configured for this tenant",
                    )
                } else {
                    (
                        "blocked",
                        "WhatsApp credentials (WHATSAPP_ACCESS_TOKEN / WHATSAPP_PHONE_NUMBER_ID) are not configured",
                    )
                };
                let intent = NotificationIntent {
                    id: format!("wa-intent-{}", Uuid::new_v4()),
                    tenant_id: tenant_id.clone(),
                    order_id: order_id.clone(),
                    phone: phone.clone(),
                    template_name: template_name.clone(),
                    status: status.to_string(),
                    reason: Some(reason.to_string()),
                    created_at: now_rfc3339(),
                };
                state.record_intent(intent.clone());
                ctx.add_result(json!({
                    "module": "whatsapp",
                    "event": ORDER_PAID_EVENT,
                    "intent_id": intent.id,
                    "order_id": order_id,
                    "status": status,
                    "reason": reason,
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.route("/api/v1/whatsapp/send", web::post().to(send_handler));
        cfg.route(
            "/api/v1/whatsapp/templates",
            web::get().to(list_templates_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/templates",
            web::post().to(create_template_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/webhook",
            web::get().to(webhook_verify_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/webhook",
            web::post().to(webhook_post_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/messages",
            web::get().to(list_messages_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/messages/{id}",
            web::get().to(get_message_handler),
        );
        cfg.route(
            "/api/v1/whatsapp/conversations",
            web::get().to(list_conversations_handler),
        );
        cfg.route("/api/v1/whatsapp/status", web::get().to(status_handler));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        // Auth notes: every route requires a valid token, except the
        // two webhook routes. Meta calls those without a token, and
        // they verify their own credential instead — the verify token
        // (GET) and the HMAC-SHA256 signature (POST) — which is
        // exactly what `RouteAuth::Public` is reserved for.
        vec![
            RouteInfo {
                method: "POST",
                path: "/api/v1/whatsapp/send",
                owner: "whatsapp",
                description: "Queue an outbound WhatsApp text or template message.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/templates",
                owner: "whatsapp",
                description: "List a tenant's WhatsApp message templates.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/whatsapp/templates",
                owner: "whatsapp",
                description: "Create a WhatsApp message template.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/webhook",
                owner: "whatsapp",
                description: "Meta webhook verification (echoes hub.challenge).",
                auth: RouteAuth::Public,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/whatsapp/webhook",
                owner: "whatsapp",
                description: "Meta webhook: inbound messages and delivery statuses.",
                auth: RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/messages",
                owner: "whatsapp",
                description: "Paginated per-tenant message log.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/messages/{id}",
                owner: "whatsapp",
                description: "One message from a tenant's log.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/conversations",
                owner: "whatsapp",
                description: "Per-phone conversation summaries with unread counts.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/whatsapp/status",
                owner: "whatsapp",
                description: "Module status (configuration flags and counts only).",
                auth: RouteAuth::Authenticated,
            },
        ]
    }

    /// Runs once at startup. Re-reads the environment (so values set
    /// after module construction are picked up) and attaches the hook
    /// bus so webhook handlers can emit `whatsapp.message.*` events.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.refresh_config_from_env();
        self.state.attach_hooks(ctx.hooks.clone());
        Ok(())
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): the root
/// `Cargo.toml` gains `external-whatsapp = { path =
/// "crates/external-whatsapp" }` in the workspace members and
/// dependencies, and `src/modules/mod.rs::register_all` gains one line
/// in its external-modules section:
/// `external_whatsapp::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WhatsAppModule::new()));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(WhatsAppModule::new())
}
