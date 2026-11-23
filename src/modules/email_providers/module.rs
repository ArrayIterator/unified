//! email_providers - module.

// Email provider registry.
//
// One hookable registry plus a common provider trait, mirroring the
// provider-registry pattern of the cloud providers module and the
// payment-gateway trait: every email provider is a typed HTTP client
// built from one stored connection, with capability flags, a real
// connection test, sandbox / test mode, and 429 handling that surfaces
// the provider's `Retry-After` delay in `error.metadata`.
//
// Providers: Mailchimp Transactional (Mandrill), Brevo, SendGrid,
// Mailgun, Postmark, Amazon SES (SigV4), Resend, SparkPost, and
// generic SMTP (which delegates to the Mail Manager's SMTP transport
// through the `mail.send_requested` hook; this module never
// re-implements SMTP delivery).
//
// Provider selection is PER send: every send call takes an explicit
// provider connection id. A global default connection may exist as a
// fallback, but no send is ever forced through it.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Provider API hosts are
// allow-listed per adapter.
//
// Routes (all under `/api/v1/email-providers`):
//   GET    /catalog                  - provider kinds, capabilities, settings fields
//   GET    /connections              - paginated connections (filter kind, enabled, search)
//   POST   /connections              - create a connection
//   GET    /connections/{id}         - one connection (masked view)
//   PUT    /connections/{id}         - update a connection (secrets are write-only)
//   DELETE /connections/{id}         - delete a connection
//   POST   /connections/{id}/test    - connection test against the real provider API
//   POST   /connections/{id}/test-send - send a real test message through the connection
//   GET    /default                  - the global default connection id (fallback only)
//   PUT    /default                  - set the global default connection id
//   POST   /send                     - send one message through an explicit connection
//   POST   /send-batch               - send one message to many recipients
//   GET    /log                      - paginated send log (filter status, provider, search)
//
// Hook contracts:
//   Answers `email.send_requested` with `{ connection_id?, to, subject,
//   body_text?, body_html?, template_id?, variables?, metadata?, from? }`
//   by queueing the message and adding a result `{ queued, job_id,
//   connection_id }` (or `{ error }`); delivery runs in a worker and
//   emits `email.sent` / `email.failed`.
//   Answers `email_provider.webhook_verify` with `{ provider,
//   connection_id?, headers, body }` by adding a result `{ valid }`
//   (or `{ error }`) using the adapter's signature check.
//   Emits `email_provider.connected`, `email_provider.connection.created`,
//   `email_provider.connection.deleted`, `email.sent`, `email.failed`.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde::{Deserialize};
use serde_json::{json};
use std::collections::HashMap;
use std::sync::atomic::{Ordering};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, provider_kinds::*, credentials::*, store::*, http::*, registry::*, adapters::*, worker::*, hooks::*, handlers::*};



// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
pub struct EmailProvidersModule {
    pub(crate) state: Arc<EmailProviderState>,
}


impl EmailProvidersModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EmailProviderState::new()),
        }
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EmailProvidersModule::new()));
}


impl Module for EmailProvidersModule {
    fn id(&self) -> &'static str {
        "email_providers"
    }

    fn name(&self) -> &'static str {
        "Email Providers"
    }

    fn description(&self) -> &'static str {
        "Email provider registry: Mailchimp Transactional, Brevo, SendGrid, Mailgun, Postmark, Amazon SES, Resend, SparkPost, and SMTP (via the Mail Manager), with per-send provider selection, connection tests, and webhook signature verification."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "email-templates"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            EVENT_EMAIL_SEND_REQUESTED,
            "email_providers",
            Arc::new(move |ctx: &mut HookContext| on_email_send_requested(ctx)),
        );
        bus.register(
            EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY,
            "email_providers",
            Arc::new(move |ctx: &mut HookContext| on_webhook_verify(ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/email-providers/catalog", owner: "email_providers", description: "Provider catalog: kinds, capabilities, settings fields, domain and webhook notes.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-providers/connections", owner: "email_providers", description: "Paginated provider connections (masked views).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections", owner: "email_providers", description: "Create a provider connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "One provider connection (masked view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "Update a provider connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "Delete a provider connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections/{id}/test", owner: "email_providers", description: "Test a connection against the real provider API.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections/{id}/test-send", owner: "email_providers", description: "Send a real test message through the connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-providers/default", owner: "email_providers", description: "The global default connection (fallback only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/email-providers/default", owner: "email_providers", description: "Set or clear the global default connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-providers/send", owner: "email_providers", description: "Send one message through an explicit provider connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-providers/send-batch", owner: "email_providers", description: "Send one message to many recipients through an explicit provider connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-providers/log", owner: "email_providers", description: "Paginated send log.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/email-providers")
                .app_data(web::Data::new(state))
                .route("/catalog", web::get().to(list_catalog))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/default", web::get().to(get_default_connection))
                .route("/default", web::put().to(set_default_connection))
                .route("/send", web::post().to(send_mail_handler))
                .route("/send-batch", web::post().to(send_batch_handler))
                .route("/log", web::get().to(list_send_log))
                .route("/connections/{id}", web::get().to(get_connection_handler))
                .route("/connections/{id}", web::put().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection_handler))
                .route("/connections/{id}/test-send", web::post().to(test_connection_send)),
        );
    }
}


#[derive(Debug, Deserialize)]
pub(crate) struct TestSendInput {
    pub(crate) to: String,
    pub(crate) subject: Option<String>,
}


pub(crate) async fn test_connection_send(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestSendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let input = body.into_inner();
    if let Err(error) = validate_email_address(&input.to) {
        return api_json::error_response(&error, debug);
    }
    let Some(connection) = get_connection(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    let message = EmailMessage {
        to: input.to.clone(),
        from: None,
        from_name: None,
        subject: input
            .subject
            .unwrap_or_else(|| "Unified test message".to_string()),
        body_text: Some("This is a test message sent from the Unified Email Providers module.".to_string()),
        body_html: Some("<p>This is a test message sent from the Unified Email Providers module.</p>".to_string()),
        reply_to: None,
        template_id: None,
        variables: HashMap::new(),
        metadata: HashMap::new(),
        test_mode: connection.test_mode,
    };
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match adapter.send(&message).await {
        Ok(result) => {
            append_log(SendLogEntry {
                id: Uuid::now_v7().to_string(),
                connection_id: connection.id.to_string(),
                provider: connection.kind.as_str().to_string(),
                to: input.to,
                subject: message.subject,
                status: if result.test_mode { "test" } else { "sent" }.to_string(),
                provider_message_id: result.provider_message_id.clone(),
                test_mode: result.test_mode,
                error: None,
                created_at: Utc::now().to_rfc3339(),
            });
            api_json::success_response(
                json!({ "sent": result.accepted, "provider": connection.kind.as_str(), "result": result }),
                200,
            )
        }
        Err(error) => {
            append_log(SendLogEntry {
                id: Uuid::now_v7().to_string(),
                connection_id: connection.id.to_string(),
                provider: connection.kind.as_str().to_string(),
                to: input.to,
                subject: message.subject,
                status: "failed".to_string(),
                provider_message_id: None,
                test_mode: message.test_mode,
                error: Some(error.message.clone()),
                created_at: Utc::now().to_rfc3339(),
            });
            api_json::error_response(&error, debug)
        }
    }
}
