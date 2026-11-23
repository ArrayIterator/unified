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
// Providers (56 built-ins, task 354): Mailchimp Transactional
// (Mandrill), Brevo, SendGrid, Mailgun, Postmark, Amazon SES
// (SigV4), Resend, SparkPost, Sender.net, SendPulse, Mailjet,
// Mailtrap, Elastic Email, SMTP2GO, MailerSend, ZeptoMail,
// SendLayer, SocketLabs, Pepipost, SMTP.com, Campaign Monitor
// (Transactional SMTP), Plunk, Courier, Loops, OneSignal Email
// (generic HTTP adapters — see `adapters.rs`), generic SMTP
// (direct lettre transport from the connection's own host, port,
// TLS mode and credentials — the old Mail Manager delegation is
// gone; see the SMTP note in `adapters.rs`), 26 mailbox SMTP
// presets (Gmail / Yahoo / Outlook, Zoho / Zoho EU, Proton,
// Yandex, Fastmail, iCloud, GMX, Mail.com, AOL, QQ, 163, 126,
// Sina, Mail.ru, Posteo, Migadu, Mailfence, StartMail, Runbox,
// Kolab Now, Infomaniak, OVH, Gandi, Namecheap, DreamHost,
// MXroute — the preset relay comes from the provider registry
// definition; a connection may override host, port, and TLS mode),
// and local sendmail (direct spawn of the sendmail binary, no
// credentials at all, the default builtin).
//
// Deliverability headers (task 354): bulk sends get
// List-Unsubscribe (mailto + the connection's unsubscribe_url
// when set), List-Unsubscribe-Post (RFC 8058 one-click),
// Precedence: bulk, X-Auto-Response-Suppress: All, and Feedback-ID
// (SMTP only); transactional sends get List-Unsubscribe only when
// the caller opts in (`include_unsubscribe`); `deliverability_opt_out`
// suppresses every header. See `deliverability.rs`.
//
// Logos: one SVG per provider in `logos/` (sprite
// `provider-logo-email-<id>`); the catalog returns the logo stem
// (`"logo": "email-gmail"`), never a URL.
//
// Provider selection is PER send: every send call takes an explicit
// provider connection id. A global default connection may exist as a
// fallback, but no send is ever forced through it.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. The OAuth secrets added
// by task 350 (SMTP password, OAuth client secret, access / refresh
// tokens) are stored ENCRYPTED (`crypto.rs`, `v1.` format, key
// `app.secret_key`); the pre-existing API-key fields keep their
// original write-only treatment. Provider API hosts are allow-listed
// per adapter.
//
// Routes (all under `/api/v1/email-providers`):
//   GET    /catalog                  - provider kinds, capabilities, settings fields, logos
//   GET    /connections              - paginated connections (filter kind, enabled, search)
//   POST   /connections              - create a connection
//   GET    /connections/{id}         - one connection (masked view)
//   PUT    /connections/{id}         - update a connection (secrets are write-only)
//   DELETE /connections/{id}         - delete a connection
//   POST   /connections/{id}/test    - connection test against the real provider API
//   POST   /connections/{id}/test-send - send a real test message through the connection
//   GET    /connections/{id}/oauth/authorize - OAuth authorization URL (Gmail/Yahoo/Outlook)
//   GET    /oauth/callback           - OAuth callback (provider redirects the browser here)
//   POST   /connections/{id}/oauth/refresh - refresh the stored OAuth tokens
//   POST   /connections/{id}/oauth/revoke - unlink the OAuth account
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
//   Emits `email_provider.settings_fields` with `{ kind, fields }`
//   when the catalog builds a provider's field list; another module
//   may add, remove, or replace fields — see
//   `settings_fields_for_catalog` in `handlers.rs` for the result
//   contract.
//   Emits `email_provider.definitions` with `{}` when the catalog
//   is built; another module may answer with one provider
//   definition object or an array of them (a `ProviderDefinition`
//   JSON object: `id`, `display_name`, `adapter_kind`, and the
//   fields the adapter needs — `smtp_host`/`smtp_port`/`smtp_tls`
//   for the generic SMTP adapter, `http_api` for the generic HTTP
//   adapter, `credentials_shape` for the settings page). Answers
//   with an invalid id, an empty display name, or an empty
//   adapter kind are skipped; a built-in wins on id collision.
//   Hook-provided definitions appear in the catalog with their
//   settings fields and description, but they have no enum kind,
//   so they cannot be used for sending until a factory is
//   registered for their adapter kind — see `registry.rs`.
//   Emits `email_provider.connected`, `email_provider.connection.created`,
//   `email_provider.connection.deleted`, `email_provider.oauth_linked`,
//   `email_provider.oauth_refreshed`, `email.sent`, `email.failed`.

use super::{
    adapters::*, credentials::*, crypto::*, events::*, handlers::*, hooks::*, http::*,
    oauth_handlers::*, provider_kinds::*, registry::*, store::*, worker::*,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use uuid::Uuid;

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
        "Email provider registry: 56 built-in providers (Mailchimp Transactional, Brevo, SendGrid, Mailgun, Postmark, Amazon SES, Resend, SparkPost, Sender.net, SendPulse, Mailjet, Mailtrap, Elastic Email, SMTP2GO, MailerSend, ZeptoMail, SendLayer, SocketLabs, Pepipost, SMTP.com, Campaign Monitor, Plunk, Courier, Loops, OneSignal, 26 mailbox SMTP presets, direct SMTP, Gmail / Yahoo / Outlook OAuth2 + XOAUTH2, and local sendmail), with per-send provider selection, connection tests, webhook signature verification, deliverability headers, and per-provider logos."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["email-templates"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        // Capture app.secret_key for OAuth secret encryption
        // (crypto.rs): no secret at boot means None, not a fallback
        // — storing an OAuth secret is refused and adapters needing
        // one fail closed.
        let secret = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .app
            .secret_key
            .clone();
        set_encryption_secret(&secret);
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
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/catalog",
                owner: "email_providers",
                description: "Provider catalog: kinds, capabilities, settings fields, domain and webhook notes.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/connections",
                owner: "email_providers",
                description: "Paginated provider connections (masked views).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/connections",
                owner: "email_providers",
                description: "Create a provider connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/connections/{id}",
                owner: "email_providers",
                description: "One provider connection (masked view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/email-providers/connections/{id}",
                owner: "email_providers",
                description: "Update a provider connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/email-providers/connections/{id}",
                owner: "email_providers",
                description: "Delete a provider connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/connections/{id}/test",
                owner: "email_providers",
                description: "Test a connection against the real provider API.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/connections/{id}/test-send",
                owner: "email_providers",
                description: "Send a real test message through the connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/connections/{id}/oauth/authorize",
                owner: "email_providers",
                description: "OAuth authorization URL for a Gmail/Yahoo/Outlook connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/oauth/callback",
                owner: "email_providers",
                description: "OAuth callback: the provider redirects the admin browser here; authorized by the single-use state token, not by a session.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/connections/{id}/oauth/refresh",
                owner: "email_providers",
                description: "Refresh the stored OAuth tokens for a connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/connections/{id}/oauth/revoke",
                owner: "email_providers",
                description: "Unlink an OAuth account and clear its stored tokens.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/default",
                owner: "email_providers",
                description: "The global default connection (fallback only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/email-providers/default",
                owner: "email_providers",
                description: "Set or clear the global default connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/send",
                owner: "email_providers",
                description: "Send one message through an explicit provider connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/email-providers/send-batch",
                owner: "email_providers",
                description: "Send one message to many recipients through an explicit provider connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/email-providers/log",
                owner: "email_providers",
                description: "Paginated send log.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
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
                .route(
                    "/connections/{id}/test",
                    web::post().to(test_connection_handler),
                )
                .route(
                    "/connections/{id}/test-send",
                    web::post().to(test_connection_send),
                )
                .route(
                    "/connections/{id}/oauth/authorize",
                    web::get().to(oauth_authorize),
                )
                .route("/oauth/callback", web::get().to(oauth_callback))
                .route(
                    "/connections/{id}/oauth/refresh",
                    web::post().to(oauth_refresh),
                )
                .route(
                    "/connections/{id}/oauth/revoke",
                    web::post().to(oauth_revoke),
                ),
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
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
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
        body_text: Some(
            "This is a test message sent from the Unified Email Providers module.".to_string(),
        ),
        body_html: Some(
            "<p>This is a test message sent from the Unified Email Providers module.</p>"
                .to_string(),
        ),
        reply_to: None,
        template_id: None,
        variables: HashMap::new(),
        metadata: HashMap::new(),
        bulk: false,
        include_unsubscribe: false,
        deliverability_opt_out: false,
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
