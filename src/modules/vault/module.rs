//! vault module - Module registration: struct, Default, Module impl, register.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::state::VaultState;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_documents::{create_document, delete_document, download_document, get_document, list_access_log, list_documents, list_versions, restore_version, update_document, upload_version};
use super::handlers_folders::{create_folder, delete_folder, get_folder, list_folder_documents, list_folders, set_folder_retention, update_folder};
use super::handlers_sharing::{approve_access_request, create_share_link, deny_access_request, flag_document_review, list_access_requests, list_share_links, request_access, resolve_shared_link, retention_review, revoke_share_link};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct VaultModule {
    pub(crate) state: VaultState,
}

impl VaultModule {
    pub fn new() -> Self {
        Self {
            state: VaultState::default(),
        }
    }
}

impl Default for VaultModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VaultModule {
    fn id(&self) -> &'static str {
        "vault"
    }

    fn name(&self) -> &'static str {
        "Document Vault"
    }

    fn description(&self) -> &'static str {
        "Secure document storage per company and client: folders, versioned documents (metadata only — file bytes live in the storage module), append-only access logs, expiring share links, access requests with approvals, and retention rules that flag for review and never auto-delete."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "storage", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "storage", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("vault", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {
        // No inbound listeners: the vault only emits events (see the
        // module header). Access decisions and share-link state are
        // driven by its own HTTP routes.
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/vault")
                .app_data(state)
                .route("/folders", web::get().to(list_folders))
                .route("/folders", web::post().to(create_folder))
                .route("/folders/{id}", web::get().to(get_folder))
                .route("/folders/{id}", web::patch().to(update_folder))
                .route("/folders/{id}", web::delete().to(delete_folder))
                .route(
                    "/folders/{id}/retention",
                    web::post().to(set_folder_retention),
                )
                .route(
                    "/folders/{id}/documents",
                    web::get().to(list_folder_documents),
                )
                .route("/documents", web::get().to(list_documents))
                .route("/documents", web::post().to(create_document))
                .route("/documents/{id}", web::get().to(get_document))
                .route("/documents/{id}", web::patch().to(update_document))
                .route("/documents/{id}", web::delete().to(delete_document))
                .route("/documents/{id}/versions", web::post().to(upload_version))
                .route("/documents/{id}/versions", web::get().to(list_versions))
                .route(
                    "/documents/{id}/versions/{version_id}/restore",
                    web::post().to(restore_version),
                )
                .route(
                    "/documents/{id}/download",
                    web::post().to(download_document),
                )
                .route("/documents/{id}/access-log", web::get().to(list_access_log))
                .route("/documents/{id}/share", web::post().to(create_share_link))
                .route(
                    "/documents/{id}/share-links",
                    web::get().to(list_share_links),
                )
                .route(
                    "/documents/{id}/request-access",
                    web::post().to(request_access),
                )
                .route(
                    "/documents/{id}/flag-review",
                    web::post().to(flag_document_review),
                )
                .route(
                    "/share-links/{id}/revoke",
                    web::post().to(revoke_share_link),
                )
                .route("/shared/{token}", web::get().to(resolve_shared_link))
                .route("/access-requests", web::get().to(list_access_requests))
                .route(
                    "/access-requests/{id}/approve",
                    web::post().to(approve_access_request),
                )
                .route(
                    "/access-requests/{id}/deny",
                    web::post().to(deny_access_request),
                )
                .route("/retention/review", web::get().to(retention_review)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/folders",
                owner: "vault",
                description: "List folders (paginated, filter by parent_id, company_id; scoped to the caller's tenant and company).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/folders",
                owner: "vault",
                description: "Create a folder (manage capability).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/folders/{id}",
                owner: "vault",
                description: "Get one folder.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/vault/folders/{id}",
                owner: "vault",
                description: "Update a folder (name, parent, retention).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/vault/folders/{id}",
                owner: "vault",
                description: "Delete an empty folder.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/folders/{id}/retention",
                owner: "vault",
                description: "Set the folder retention rule (retain N days, flag for review).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/folders/{id}/documents",
                owner: "vault",
                description: "List documents in a folder (visibility enforced).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/documents",
                owner: "vault",
                description: "List visible documents (paginated, filter by folder_id, status, company_id, flagged_for_review; scoped to the caller's tenant — company_id is the resolved company, never a client-supplied scope). Clients only see shared documents.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents",
                owner: "vault",
                description: "Upload document metadata (storage target + key, checksum, size); creates version 1.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/documents/{id}",
                owner: "vault",
                description: "Get one document (logged as a view; invisible documents answer 404).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/vault/documents/{id}",
                owner: "vault",
                description: "Update a document (name, description, folder, status, shared clients).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/vault/documents/{id}",
                owner: "vault",
                description: "Delete a document (explicit, audited; file bytes stay with the storage module).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/versions",
                owner: "vault",
                description: "Upload a new version (metadata); old versions are kept.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/documents/{id}/versions",
                owner: "vault",
                description: "List versions of a document (oldest first).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/versions/{version_id}/restore",
                owner: "vault",
                description: "Restore an old version as the current version.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/download",
                owner: "vault",
                description: "Record a download (logged) and return the storage reference; bytes are served by the storage module.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/documents/{id}/access-log",
                owner: "vault",
                description: "List the append-only access log of a document (filter by action, actor_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/share",
                owner: "vault",
                description: "Create an expiring share link (unguessable token, download cap).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/documents/{id}/share-links",
                owner: "vault",
                description: "List share links of a document (tokens are never returned here).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/request-access",
                owner: "vault",
                description: "Request access to a document (owner is notified).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/documents/{id}/flag-review",
                owner: "vault",
                description: "Flag a document for retention review.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/share-links/{id}/revoke",
                owner: "vault",
                description: "Revoke a share link immediately.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/shared/{token}",
                owner: "vault",
                description: "PUBLIC: resolve a share link by token (no auth; gated by the API Manager ApiAccessRule for this path; revoked/expired/exhausted links answer 404).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/access-requests",
                owner: "vault",
                description: "List access requests (managers see all; others see their own and requests for documents they own).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/access-requests/{id}/approve",
                owner: "vault",
                description: "Approve an access request (audited; requester is notified).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/vault/access-requests/{id}/deny",
                owner: "vault",
                description: "Deny an access request (audited; requester is notified).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/vault/retention/review",
                owner: "vault",
                description: "List documents past their retention window or flagged for review (never auto-deleted).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(VaultModule::new()));
}
