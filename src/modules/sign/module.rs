//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_envelopes::{
    cancel_envelope, create_envelope, delete_envelope, get_certificate, get_envelope, list_audit,
    list_envelopes, list_reminders_due, remind_envelope, send_envelope, update_envelope,
    void_envelope,
};
use super::handlers_settings::{get_settings, update_settings};
use super::handlers_signers::{
    add_signer, delete_signer, get_sign_page, get_signer, list_signers, post_decline, post_sign,
    resend_signer, update_signer, verify_certificate,
};
use super::handlers_templates::{
    create_template, delete_template, get_template, list_templates, update_template, use_template,
};
use super::hooks::{on_document_deleted, on_user_deleted};
use super::state::SignState;
use super::types::{EVENT_DOCUMENT_DELETED, EVENT_USER_DELETED};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SignModule {
    pub(crate) state: SignState,
}

impl SignModule {
    pub fn new() -> Self {
        Self {
            state: SignState::default(),
        }
    }
}

impl Default for SignModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SignModule {
    fn id(&self) -> &'static str {
        "sign"
    }

    fn name(&self) -> &'static str {
        "Sign"
    }

    fn description(&self) -> &'static str {
        "Core e-signature: signing requests (envelopes) over multiple documents-module documents with sequential or parallel signing order and expiry; signers with hashed signing tokens (raw token returned once, resend rotates), statuses pending, sent, viewed, signed, and declined; signature capture as drawn strokes, typed names, or uploaded images — only SHA-256 hashes are stored, plus the hash of the signed document and the signed-at time for tamper evidence; reusable templates with signer roles and fields as data; reminder schedules as data with due computation and reminder events for the notifications module; completion certificates bound to the hash-chained audit trail; and public token-gated sign and certificate-verify flows."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "documents"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "documents"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("sign", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_DOCUMENT_DELETED,
            "sign",
            Arc::new(move |ctx: &mut HookContext| on_document_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "sign",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/sign")
                .app_data(state)
                .route("/public/sign/{token}", web::get().to(get_sign_page))
                .route("/public/sign/{token}", web::post().to(post_sign))
                .route("/public/sign/{token}/decline", web::post().to(post_decline))
                .route(
                    "/public/verify/{certificate_hash}",
                    web::get().to(verify_certificate),
                )
                .route("/envelopes", web::get().to(list_envelopes))
                .route("/envelopes", web::post().to(create_envelope))
                .route("/envelopes/{envelope_id}", web::get().to(get_envelope))
                .route("/envelopes/{envelope_id}", web::patch().to(update_envelope))
                .route(
                    "/envelopes/{envelope_id}",
                    web::delete().to(delete_envelope),
                )
                .route(
                    "/envelopes/{envelope_id}/send",
                    web::post().to(send_envelope),
                )
                .route(
                    "/envelopes/{envelope_id}/cancel",
                    web::post().to(cancel_envelope),
                )
                .route(
                    "/envelopes/{envelope_id}/void",
                    web::post().to(void_envelope),
                )
                .route(
                    "/envelopes/{envelope_id}/certificate",
                    web::get().to(get_certificate),
                )
                .route("/envelopes/{envelope_id}/audit", web::get().to(list_audit))
                .route(
                    "/envelopes/{envelope_id}/remind",
                    web::post().to(remind_envelope),
                )
                .route("/reminders/due", web::get().to(list_reminders_due))
                .route(
                    "/envelopes/{envelope_id}/signers",
                    web::get().to(list_signers),
                )
                .route(
                    "/envelopes/{envelope_id}/signers",
                    web::post().to(add_signer),
                )
                .route(
                    "/envelopes/{envelope_id}/signers/{signer_id}",
                    web::get().to(get_signer),
                )
                .route(
                    "/envelopes/{envelope_id}/signers/{signer_id}",
                    web::patch().to(update_signer),
                )
                .route(
                    "/envelopes/{envelope_id}/signers/{signer_id}",
                    web::delete().to(delete_signer),
                )
                .route(
                    "/envelopes/{envelope_id}/signers/{signer_id}/resend",
                    web::post().to(resend_signer),
                )
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{template_id}", web::get().to(get_template))
                .route("/templates/{template_id}", web::patch().to(update_template))
                .route(
                    "/templates/{template_id}",
                    web::delete().to(delete_template),
                )
                .route("/templates/{template_id}/use", web::post().to(use_template))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/public/sign/{token}",
                owner: "sign",
                description: "PUBLIC (token-gated): sign page data for one signing token — envelope summary, the signer, other signers' progress, and allowed capture methods.",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/public/sign/{token}",
                owner: "sign",
                description: "PUBLIC (token-gated): record a signature through a signing token — signature hash and document hash only, checked against the hashes recorded at send time.",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/public/sign/{token}/decline",
                owner: "sign",
                description: "PUBLIC (token-gated): decline through a signing token; the envelope is declined and outstanding tokens are revoked.",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/public/verify/{certificate_hash}",
                owner: "sign",
                description: "PUBLIC: verify a completion certificate — recompute its hash from the signing record and verify the audit chain.",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes",
                owner: "sign",
                description: "List envelopes (paginated, filter by status, signer, document_id; search by title/message).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes",
                owner: "sign",
                description: "Create a draft envelope with documents and inline signers (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes/{envelope_id}",
                owner: "sign",
                description: "Get one envelope with its signers.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sign/envelopes/{envelope_id}",
                owner: "sign",
                description: "Update a draft envelope (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sign/envelopes/{envelope_id}",
                owner: "sign",
                description: "Delete an envelope that is not live or completed, with its signers and audit trail (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/send",
                owner: "sign",
                description: "Send a draft envelope: every document needs its hash, tokens are issued to all signers (raw tokens returned once), signers are notified (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/cancel",
                owner: "sign",
                description: "Cancel a live envelope and revoke its outstanding tokens (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/void",
                owner: "sign",
                description: "Void a live envelope and revoke its outstanding tokens (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes/{envelope_id}/certificate",
                owner: "sign",
                description: "Get the completion certificate of a completed envelope with its audit-chain check.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes/{envelope_id}/audit",
                owner: "sign",
                description: "List the hash-chained audit trail of an envelope (paginated, filter by action) with the chain verification result.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/remind",
                owner: "sign",
                description: "Send reminders now to every signer who has not signed yet; emits sign.reminder.due per signer (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/reminders/due",
                owner: "sign",
                description: "List envelopes whose next reminder is due (read-only; nothing is sent).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers",
                owner: "sign",
                description: "List the signers of an envelope (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers",
                owner: "sign",
                description: "Add a signer to an envelope; a signer added to a sent envelope is sent immediately with a fresh token (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers/{signer_id}",
                owner: "sign",
                description: "Get one signer of an envelope (token hash never exposed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers/{signer_id}",
                owner: "sign",
                description: "Update a signer of a draft envelope (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers/{signer_id}",
                owner: "sign",
                description: "Remove a signer from a draft envelope, or a pending signer from a live one (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/envelopes/{envelope_id}/signers/{signer_id}/resend",
                owner: "sign",
                description: "Rotate a signer's token and resend the signing link (raw token returned once) (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/templates",
                owner: "sign",
                description: "List signing templates (paginated, search by name/description).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/templates",
                owner: "sign",
                description: "Create a signing template with documents, signer roles, and fields as data (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/templates/{template_id}",
                owner: "sign",
                description: "Get one signing template.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sign/templates/{template_id}",
                owner: "sign",
                description: "Update a signing template (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/sign/templates/{template_id}",
                owner: "sign",
                description: "Delete a signing template; envelopes built from it keep their own snapshot (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/sign/templates/{template_id}/use",
                owner: "sign",
                description: "Build a draft envelope from a template, merging template-role defaults with supplied signers (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/sign/settings",
                owner: "sign",
                description: "Get per-tenant sign settings (allowed capture methods, default expiry, reminder defaults, provider).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/sign/settings",
                owner: "sign",
                description: "Update per-tenant sign settings (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SignModule::new()));
}
