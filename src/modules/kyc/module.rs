//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::{ai_capability, verify_case};
use super::handlers_cases::{
    create_case, delete_case, get_case, get_case_status, list_case_audit, list_cases, submit_case,
    update_case,
};
use super::handlers_config::{
    create_context, create_document_type, create_instruction, create_risk_level,
    create_subject_type, delete_context, delete_document_type, delete_instruction,
    delete_risk_level, delete_subject_type, get_context, get_document_type, get_instruction,
    get_resolved_context, get_risk_level, get_settings, get_subject_type, list_contexts,
    list_document_types, list_instructions, list_risk_levels, list_subject_types, update_context,
    update_document_type, update_instruction, update_risk_level, update_settings,
    update_subject_type,
};
use super::handlers_documents::{get_document, link_document, list_documents, unlink_document};
use super::handlers_review::{
    approve_case, escalate_case, expire_case, list_queue, reject_case, request_more_info,
};
use super::hooks::{
    ai_verification_event, mcp_context_event, on_ai_verification_requested, on_mcp_context_kyc,
    on_user_deleted,
};
use super::state::KycState;
use super::types::EVENT_USER_DELETED;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// Module

pub struct KycModule {
    pub(crate) state: KycState,
}

impl KycModule {
    pub fn new() -> Self {
        Self {
            state: KycState::default(),
        }
    }
}

impl Default for KycModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for KycModule {
    fn id(&self) -> &'static str {
        "kyc"
    }

    fn name(&self) -> &'static str {
        "KYC"
    }

    fn description(&self) -> &'static str {
        "Know Your Customer: identity verification cases with linked identity documents (references and hashes only, never file bytes), an AI verification pipeline that runs under a stored, admin-editable default verification context merged with versioned extra instructions through the AI module's gated runner, a human admin review queue (approve, reject, request more information, escalate, expire) where the AI verdict never approves a case on its own, risk levels as data, subject and document types as data, and a hash-chained audit trail."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "documents", "ai", "mcp"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "documents", "ai", "mcp"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("compliance")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("kyc", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "kyc",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        // The AI module's runner emits this grounding event for
        // the verification capability; this module answers with
        // the resolved default context, the active extra
        // instructions, and the case's document metadata.
        let state = self.state.clone();
        bus.register(
            ai_verification_event(),
            "kyc",
            Arc::new(move |ctx: &mut HookContext| on_ai_verification_requested(state.clone(), ctx)),
        );
        // The MCP module's context contract: `mcp.context.kyc`
        // serves per-tenant case summaries to MCP servers.
        let state = self.state.clone();
        bus.register(
            mcp_context_event(),
            "kyc",
            Arc::new(move |ctx: &mut HookContext| on_mcp_context_kyc(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/kyc")
                .app_data(state)
                .route("/cases", web::get().to(list_cases))
                .route("/cases", web::post().to(create_case))
                .route("/cases/{case_id}", web::get().to(get_case))
                .route("/cases/{case_id}", web::patch().to(update_case))
                .route("/cases/{case_id}", web::delete().to(delete_case))
                .route("/cases/{case_id}/submit", web::post().to(submit_case))
                .route("/cases/{case_id}/status", web::get().to(get_case_status))
                .route("/cases/{case_id}/audit", web::get().to(list_case_audit))
                .route("/cases/{case_id}/documents", web::get().to(list_documents))
                .route("/cases/{case_id}/documents", web::post().to(link_document))
                .route(
                    "/cases/{case_id}/documents/{document_id}",
                    web::get().to(get_document),
                )
                .route(
                    "/cases/{case_id}/documents/{document_id}",
                    web::delete().to(unlink_document),
                )
                .route("/cases/{case_id}/verify", web::post().to(verify_case))
                .route("/cases/{case_id}/approve", web::post().to(approve_case))
                .route("/cases/{case_id}/reject", web::post().to(reject_case))
                .route(
                    "/cases/{case_id}/request-more-info",
                    web::post().to(request_more_info),
                )
                .route("/cases/{case_id}/escalate", web::post().to(escalate_case))
                .route("/cases/{case_id}/expire", web::post().to(expire_case))
                .route("/admin/queue", web::get().to(list_queue))
                .route("/ai/{capability}", web::post().to(ai_capability))
                .route("/document-types", web::get().to(list_document_types))
                .route("/document-types", web::post().to(create_document_type))
                .route("/document-types/{row_id}", web::get().to(get_document_type))
                .route(
                    "/document-types/{row_id}",
                    web::patch().to(update_document_type),
                )
                .route(
                    "/document-types/{row_id}",
                    web::delete().to(delete_document_type),
                )
                .route("/subject-types", web::get().to(list_subject_types))
                .route("/subject-types", web::post().to(create_subject_type))
                .route("/subject-types/{row_id}", web::get().to(get_subject_type))
                .route(
                    "/subject-types/{row_id}",
                    web::patch().to(update_subject_type),
                )
                .route(
                    "/subject-types/{row_id}",
                    web::delete().to(delete_subject_type),
                )
                .route("/contexts", web::get().to(list_contexts))
                .route("/contexts", web::post().to(create_context))
                .route("/contexts/resolved", web::get().to(get_resolved_context))
                .route("/contexts/{row_id}", web::get().to(get_context))
                .route("/contexts/{row_id}", web::patch().to(update_context))
                .route("/contexts/{row_id}", web::delete().to(delete_context))
                .route("/instructions", web::get().to(list_instructions))
                .route("/instructions", web::post().to(create_instruction))
                .route("/instructions/{row_id}", web::get().to(get_instruction))
                .route(
                    "/instructions/{row_id}",
                    web::patch().to(update_instruction),
                )
                .route(
                    "/instructions/{row_id}",
                    web::delete().to(delete_instruction),
                )
                .route("/risk-levels", web::get().to(list_risk_levels))
                .route("/risk-levels", web::post().to(create_risk_level))
                .route("/risk-levels/{row_id}", web::get().to(get_risk_level))
                .route("/risk-levels/{row_id}", web::patch().to(update_risk_level))
                .route("/risk-levels/{row_id}", web::delete().to(delete_risk_level))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases",
                owner: "kyc",
                description: "List KYC cases (paginated, filter by status, risk level, subject type, subject; subjects see only their own).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases",
                owner: "kyc",
                description: "Open a draft KYC case for a subject type defined as data (manager may open for another subject).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases/{case_id}",
                owner: "kyc",
                description: "Get one KYC case with its linked documents and audit trail (subject, submitter, or manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/cases/{case_id}",
                owner: "kyc",
                description: "Update a draft KYC case (subject or subject type).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/cases/{case_id}",
                owner: "kyc",
                description: "Delete a draft KYC case with its linked documents and audit trail.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/submit",
                owner: "kyc",
                description: "Submit a draft case (draft -> submitted); all documents required by its subject type must be linked.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases/{case_id}/status",
                owner: "kyc",
                description: "Lightweight status read for a case: status, risk score and level, AI verdict summary, expiry.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases/{case_id}/audit",
                owner: "kyc",
                description: "List the hash-chained audit trail of a case (paginated, filter by action) with the chain verification result.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases/{case_id}/documents",
                owner: "kyc",
                description: "List the documents linked to a case (paginated, filter by document type code).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/documents",
                owner: "kyc",
                description: "Link a documents-module document to a case by reference (document id plus SHA-256 hash, never file bytes); changing documents invalidates any earlier AI verdict.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/cases/{case_id}/documents/{document_id}",
                owner: "kyc",
                description: "Get one document linked to a case.",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/cases/{case_id}/documents/{document_id}",
                owner: "kyc",
                description: "Unlink a document from a case; changing documents invalidates any earlier AI verdict.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/verify",
                owner: "kyc",
                description: "Run AI verification for a case (manager): the active default verification context merged with its extra instructions is sent through the AI module's gated runner; the verdict and risk score are stored and the case returns to the review queue.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/approve",
                owner: "kyc",
                description: "Approve a case (manager, human decision; the AI verdict never approves on its own).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/reject",
                owner: "kyc",
                description: "Reject a case with a required reason (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/request-more-info",
                owner: "kyc",
                description: "Request more information on a case with a reason and optional requested document types; the subject is notified (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/escalate",
                owner: "kyc",
                description: "Escalate a non-terminal case for senior review (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/cases/{case_id}/expire",
                owner: "kyc",
                description: "Expire a case (manager); the subject is notified.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/admin/queue",
                owner: "kyc",
                description: "Admin review queue (manager): cases with counts by status, filter by status, risk level, subject type, subject.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/ai/{capability}",
                owner: "kyc",
                description: "Run one registered KYC AI capability through the AI module's gated runner.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/document-types",
                owner: "kyc",
                description: "List document types (manager; paginated, filter by active and subject type, search by code/label).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/document-types",
                owner: "kyc",
                description: "Create a document type as data (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/document-types/{row_id}",
                owner: "kyc",
                description: "Get one document type (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/document-types/{row_id}",
                owner: "kyc",
                description: "Update a document type (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/document-types/{row_id}",
                owner: "kyc",
                description: "Delete a document type with no linked documents (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/subject-types",
                owner: "kyc",
                description: "List subject types (manager; paginated, filter by active, search by code/label).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/subject-types",
                owner: "kyc",
                description: "Create a subject type as data with its required document types (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/subject-types/{row_id}",
                owner: "kyc",
                description: "Get one subject type (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/subject-types/{row_id}",
                owner: "kyc",
                description: "Update a subject type (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/subject-types/{row_id}",
                owner: "kyc",
                description: "Delete a subject type with no cases (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/contexts",
                owner: "kyc",
                description: "List verification contexts (manager; paginated, filter by active, search by name).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/contexts",
                owner: "kyc",
                description: "Create a verification context as data (manager); activating it deactivates the other active context.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/contexts/{row_id}",
                owner: "kyc",
                description: "Get one verification context (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/contexts/{row_id}",
                owner: "kyc",
                description: "Update a verification context (manager); content changes bump its version.",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/contexts/{row_id}",
                owner: "kyc",
                description: "Delete a verification context with no instructions (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/contexts/resolved",
                owner: "kyc",
                description: "Get the resolved default verification context for this tenant: the active context merged with its active extra instructions (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/instructions",
                owner: "kyc",
                description: "List extra instructions (manager; paginated, filter by active and context, search by text).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/instructions",
                owner: "kyc",
                description: "Create an extra instruction as data, optionally scoped to one context (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/instructions/{row_id}",
                owner: "kyc",
                description: "Get one extra instruction (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/instructions/{row_id}",
                owner: "kyc",
                description: "Update an extra instruction (manager); changing the text creates a new version and deactivates the old row.",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/instructions/{row_id}",
                owner: "kyc",
                description: "Delete an extra instruction (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/risk-levels",
                owner: "kyc",
                description: "List risk levels (manager; paginated, filter by active, search by code/label).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/kyc/risk-levels",
                owner: "kyc",
                description: "Create a risk level as data (manager); active ranges must not overlap.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/risk-levels/{row_id}",
                owner: "kyc",
                description: "Get one risk level (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/risk-levels/{row_id}",
                owner: "kyc",
                description: "Update a risk level (manager); active ranges must not overlap.",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/kyc/risk-levels/{row_id}",
                owner: "kyc",
                description: "Delete a risk level assigned to no cases (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/kyc/settings",
                owner: "kyc",
                description: "Get per-tenant KYC settings (manager).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/kyc/settings",
                owner: "kyc",
                description: "Update per-tenant KYC settings, including the auto-approve low-risk policy (manager; defaults to off).",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(KycModule::new()));
}
