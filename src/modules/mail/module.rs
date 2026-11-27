//! Mail - module struct, route table and registration.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use crate::modules::ai;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use super::ai_capabilities::AI_CAPABILITIES;
use super::ai::ai_capability;
use super::handlers::get_settings;
use super::handlers::list_connectors;
use super::handlers::list_log;
use super::handlers::send_mail;
use super::handlers::test_mail;
use super::handlers::update_settings;
use super::service::deliver_queued;
use super::service::parse_mailbox;
use super::service::validate_header_value;
use super::state::MailState;
use super::types::CONNECTOR_SMTP;
use super::types::EVENT_MAIL_DELIVER;
use super::types::EVENT_MAIL_SEND_REQUESTED;
use super::types::EVENT_SCHEDULER_ENQUEUE;
use super::types::MailLogEntry;
use super::types::QueuedMail;

// Module

pub struct MailModule {
    state: Arc<MailState>,
}

impl MailModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MailState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MailModule::new()));
}

impl Module for MailModule {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn name(&self) -> &'static str {
        "Mail Manager"
    }

    fn description(&self) -> &'static str {
        "SMTP settings, mail sending, send log, and delivery connectors (SMTP, local sendmail, log-only debug)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        ai::register_ai_capabilities("mail", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Incoming send requests from Billing, Tickets, Auth, Notifications
        // and Email Templates.
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_SEND_REQUESTED,
            "mail",
            Arc::new(move |ctx| {
                let payload = ctx.data.clone();
                let to = payload
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let subject = payload
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let body_text = payload
                    .get("body_text")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let body_html = payload
                    .get("body_html")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let template_id = payload
                    .get("template_id")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let variables: HashMap<String, Value> = payload
                    .get("variables")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                if to.trim().is_empty() || (subject.trim().is_empty() && template_id.is_none()) {
                    ctx.add_result(json!({
                        "error": "mail.send_requested requires 'to' and a subject or template_id"
                    }));
                    return;
                }
                // Validate the recipient and the subject before queueing:
                // the hook payload is untrusted input, and a CR or LF in
                // either would inject arbitrary headers into the
                // local-sendmail message at delivery time.
                if let Err(error) = parse_mailbox(&to) {
                    ctx.add_result(json!({ "error": error.message }));
                    return;
                }
                if let Err(error) = validate_header_value("subject", &subject) {
                    ctx.add_result(json!({ "error": error.message }));
                    return;
                }
                let job_id = Uuid::now_v7().to_string();
                let job = QueuedMail {
                    job_id: job_id.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    body_text,
                    body_html,
                    template_id: template_id.clone(),
                    variables,
                };
                let connector = state
                    .settings_snapshot()
                    .map(|settings| settings.connector)
                    .unwrap_or_else(|| CONNECTOR_SMTP.to_string());
                state.append_log(MailLogEntry {
                    id: job_id.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    status: "queued".to_string(),
                    connector,
                    template_id,
                    error: None,
                    created_at: Utc::now().to_rfc3339(),
                });
                state.enqueue(job.clone());

                if let Some(hooks) = state.hooks() {
                    // Hand execution to the Scheduler module; delivery is
                    // the scheduler's responsibility from here. The
                    // scheduler calls the job back through `mail.deliver`.
                    hooks.emit(
                        EVENT_SCHEDULER_ENQUEUE,
                        json!({
                            "job_id": job_id,
                            "queue": "mail",
                            "event": EVENT_MAIL_SEND_REQUESTED,
                            "payload": payload,
                        }),
                    );
                    ctx.add_result(json!({
                        "queued": true,
                        "job_id": job_id,
                        "via": "scheduler"
                    }));
                } else {
                    // No hook bus: deliver directly in a worker thread and
                    // log the real outcome.
                    let worker_state = state.clone();
                    std::thread::spawn(move || deliver_queued(&worker_state, job));
                    ctx.add_result(json!({
                        "queued": true,
                        "job_id": job_id,
                        "via": "direct"
                    }));
                }
            }),
        );

        // Delivery callback used by the Scheduler module (or any other
        // module) to execute a queued job.
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_DELIVER,
            "mail",
            Arc::new(move |ctx| {
                let job_id = ctx
                    .get("job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match state.take_queued(&job_id) {
                    Some(job) => {
                        let worker_state = state.clone();
                        std::thread::spawn(move || deliver_queued(&worker_state, job));
                        ctx.add_result(json!({ "delivering": true, "job_id": job_id }));
                    }
                    None => {
                        ctx.add_result(json!({
                            "error": format!("Queued mail job not found: {job_id}")
                        }));
                    }
                }
            }),
        );

        // Ground the `draft` AI capability. This module holds no
        // groundable store: the subject and bullet points come from
        // the caller, and the send log holds recipient addresses, so
        // it is never offered as grounding. The empty result keeps
        // the hook contract explicit and `grounded` false. Tenant
        // scoping is vacuous here: no tenant data is read at all.
        bus.register(
            "mail.ai.draft_requested",
            "mail",
            Arc::new(move |ctx| {
                ctx.add_result(json!({}));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/mail/settings",
                owner: "mail",
                description: "Get settings",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/mail/settings",
                owner: "mail",
                description: "Update settings",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/mail/send",
                owner: "mail",
                description: "Send mail",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/mail/test",
                owner: "mail",
                description: "Test mail",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/mail/log",
                owner: "mail",
                description: "List log",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/mail/connectors",
                owner: "mail",
                description: "List connectors",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/mail/ai/{capability}",
                owner: "mail",
                description: "Run one registered mail AI capability (draft)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/mail")
                .app_data(web::Data::new(state))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/send", web::post().to(send_mail))
                .route("/test", web::post().to(test_mail))
                .route("/log", web::get().to(list_log))
                .route("/connectors", web::get().to(list_connectors))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }
}
