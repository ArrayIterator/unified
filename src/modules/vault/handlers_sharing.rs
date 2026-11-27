//! vault module - HTTP handlers for share links, access requests and retention.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::access::can_see_document;
use super::access::load_managed_document;
use super::access::load_visible_document;
use super::access::new_share_token;
use super::access::require_document_owner_or_scoped_admin;
use super::constants::ACTION_APPROVE;
use super::constants::ACTION_DENY;
use super::constants::ACTION_DOWNLOAD;
use super::constants::ACTION_FLAG_REVIEW;
use super::constants::ACTION_LINK_REVOKE;
use super::constants::ACTION_REQUEST;
use super::constants::ACTION_SHARE;
use super::constants::MAX_LINK_DOWNLOADS;
use super::constants::MAX_REASON_LEN;
use super::constants::PERM_VAULT_MANAGE;
use super::constants::PERM_VAULT_VIEW;
use super::helpers::caller;
use super::helpers::check_capability;
use super::helpers::created;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::parse_uuid;
use super::helpers::require_permission;
use super::helpers::require_text;
use super::helpers::resolve_tenant;
use super::helpers::Caller;
use super::state::VaultState;
use super::types::CreateShareLinkRequest;
use super::types::DecideAccessRequest;
use super::types::RequestAccessRequest;
use super::types::VaultAccessRequest;
use super::types::VaultDocument;
use super::types::VaultShareLink;

// Share-link handlers

pub(crate) async fn create_share_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateShareLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultShareLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        if input.expires_in_seconds < 60 || input.expires_in_seconds > 365 * 24 * 60 * 60 {
            return Err(UnifiedError::bad_request(
                "expires_in_seconds must be between 60 and 31536000 (one year)",
            ));
        }
        if input.max_downloads < 1 || input.max_downloads > MAX_LINK_DOWNLOADS {
            return Err(UnifiedError::bad_request(format!(
                "max_downloads must be between 1 and {MAX_LINK_DOWNLOADS}"
            )));
        }
        let now = Utc::now();
        let link = VaultShareLink {
            id: Uuid::now_v7(),
            document_id: document.id,
            token: new_share_token(),
            tenant_id: Some(caller.tenant_id.clone()),
            expires_at: now + chrono::Duration::seconds(input.expires_in_seconds),
            max_downloads: input.max_downloads,
            download_count: 0,
            revoked: false,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // Inserts the row and its token-hash index entry together.
        state.insert_share_link(link.clone());
        state.log_access(document.id, &caller.actor, ACTION_SHARE, None);
        state.emit(
            "vault.link.created",
            json!({
                "document_id": document.id.to_string(),
                "link_id": link.id.to_string(),
                "expires_at": link.expires_at.to_rfc3339(),
                "max_downloads": link.max_downloads,
                "created_by": caller.actor,
            }),
        );
        state.emit(
            "vault.document.shared",
            json!({
                "document_id": document.id.to_string(),
                "link_id": link.id.to_string(),
                "shared_by": caller.actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_share_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        let params = query.into_inner();
        let revoked_filter = params.filters.get("revoked").cloned();
        let mut items: Vec<VaultShareLink> = state
            .share_links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| {
                if link.document_id != document.id {
                    return false;
                }
                if let Some(revoked) = &revoked_filter {
                    let expected = match revoked.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if link.revoked != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|link| link.created_at);
        // The token is the link credential: it is only returned on
        // creation. List responses carry the link metadata without
        // the token so a leaked list response cannot resolve links.
        let safe: Vec<Value> = items
            .into_iter()
            .map(|link| {
                json!({
                    "id": link.id.to_string(),
                    "document_id": link.document_id.to_string(),
                    "expires_at": link.expires_at.to_rfc3339(),
                    "max_downloads": link.max_downloads,
                    "download_count": link.download_count,
                    "revoked": link.revoked,
                    "created_by": link.created_by,
                    "created_at": link.created_at.to_rfc3339(),
                    "updated_at": link.updated_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(paginate(safe, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn revoke_share_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "share link")?;
        let link = state
            .share_links
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|link| link.tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
            .ok_or_else(|| UnifiedError::not_found(format!("Share link not found: {id}")))?;
        // Only the document owner or a scoped admin for the
        // document's company may revoke its links: holding
        // vault.manage alone is not enough. The ownership check
        // runs before the revoked check so a non-owner learns
        // nothing about the link's state.
        let document = state
            .documents
            .get(&link.document_id)
            .map(|entry| entry.value().clone())
            .filter(|document| document.tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Document not found: {}", link.document_id))
            })?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        require_document_owner_or_scoped_admin(&document, &caller, is_manager)?;
        if link.revoked {
            return Err(UnifiedError::conflict("Share link is already revoked"));
        }
        let mut updated = link.clone();
        updated.revoked = true;
        updated.updated_at = Utc::now();
        // Token unchanged, so the hash-index entry is unaffected;
        // still routed through the shared helper so the index can
        // never drift from the rows.
        state.insert_share_link(updated.clone());
        state.log_access(link.document_id, &caller.actor, ACTION_LINK_REVOKE, None);
        state.emit(
            "vault.link.revoked",
            json!({
                "document_id": link.document_id.to_string(),
                "link_id": link.id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        Ok(json!({ "revoked": true, "link_id": link.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve a share link by its token. Public route: no caller
/// headers are required, and the API Manager's `ApiAccessRule`
/// gate decides whether the path is public at all (see the module
/// header). The token is never logged. A revoked, expired, or
/// exhausted link answers 404, the same as an unknown token, so
/// link state never leaks. The request must still resolve to the
/// link's tenant (same rule as the calendar module's public feed):
/// no tenant context, or a foreign tenant, answers 404.
/// The download count is checked and incremented atomically under
/// the entry lock (`get_mut` on the link row): a clone-then-insert
/// sequence would let two concurrent requests read the same count
/// and both pass, exceeding `max_downloads`.
pub(crate) async fn resolve_shared_link(
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let token = path.into_inner();
    let tenant_id = match resolve_tenant(&req) {
        Ok(tenant) => tenant.tenant_id.to_string(),
        Err(_) => return err(UnifiedError::not_found("Share link not found"), false),
    };
    let now = Utc::now();
    // Hash-index lookup: no scan over all links, no token
    // comparison against stored tokens.
    let Some(link_id) = state.lookup_share_link_id(&token) else {
        return err(UnifiedError::not_found("Share link not found"), false);
    };
    let Some(mut link) = state.share_links.get_mut(&link_id) else {
        return err(UnifiedError::not_found("Share link not found"), false);
    };
    if link.tenant_id.as_deref() != Some(tenant_id.as_str())
        || link.revoked
        || now > link.expires_at
        || link.download_count >= link.max_downloads
    {
        return err(UnifiedError::not_found("Share link not found"), false);
    }
    let Some(document) = state
        .documents
        .get(&link.document_id)
        .map(|entry| entry.value().clone())
    else {
        return err(UnifiedError::not_found("Share link not found"), false);
    };
    if document.status != "active" {
        return err(UnifiedError::not_found("Share link not found"), false);
    }
    // Still holding the entry lock: the check above and this
    // increment are one atomic step, so max_downloads can never
    // be exceeded by concurrent resolutions.
    link.download_count += 1;
    let updated = (*link).clone();
    drop(link);
    state.log_access(
        document.id,
        "share-link",
        ACTION_DOWNLOAD,
        Some(format!("via share link {}", updated.id)),
    );
    ok(json!({
        "document_id": document.id.to_string(),
        "name": document.name,
        "storage_target": document.storage_target,
        "storage_key": document.storage_key,
        "checksum_sha256": document.checksum_sha256,
        "size_bytes": document.size_bytes,
        "mime_type": document.mime_type,
        "version_id": document.current_version_id.map(|id| id.to_string()),
        "downloads_remaining": updated.max_downloads - updated.download_count,
        "expires_at": updated.expires_at.to_rfc3339(),
    }))
}

// Access-request handlers

pub(crate) async fn request_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RequestAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultAccessRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let raw_id = path.into_inner();
        let document = load_visible_document(&ctx, &state, &raw_id, &caller).or_else(|_| {
            // DELIBERATE TRADE-OFF (kept intentionally - see below):
            // a requester who cannot see the document yet may still
            // request access, so the document is loaded without the
            // visibility gate. The cost is that document existence
            // leaks through this endpoint: an existing-but-invisible
            // document yields 201 (or 409 for a duplicate request)
            // while a missing document yields 404. That leak is
            // accepted because the alternative breaks the endpoint:
            // answering 404 for invisible documents would make it
            // impossible to request access to exactly the documents
            // one cannot see yet, and answering 201 unconditionally
            // would create phantom requests for ids that do not
            // exist. The leak is narrow - only existence, never
            // metadata: no name, owner, or content is returned, the
            // owner alone is notified, and the request itself is
            // still decided by the owner. Tenant scope is NOT part
            // of the trade-off: a document from another tenant
            // still answers 404, exactly like a missing one.
            let id = parse_uuid(&raw_id, "document")?;
            state
                .documents
                .get(&id)
                .map(|entry| entry.value().clone())
                .filter(|document| document.tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
                .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))
        })?;
        if document.owner_id == caller.actor {
            return Err(UnifiedError::conflict(
                "The document owner already has access",
            ));
        }
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        if can_see_document(
            &state,
            &document,
            &caller.actor,
            is_manager,
            &caller.tenant_id,
        ) {
            return Err(UnifiedError::conflict(
                "Access is already granted for this document",
            ));
        }
        if state.access_requests.iter().any(|entry| {
            let request = entry.value();
            request.document_id == document.id
                && request.requester_id == caller.actor
                && request.status == "pending"
        }) {
            return Err(UnifiedError::conflict(
                "An access request for this document is already pending",
            ));
        }
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let request = VaultAccessRequest {
            id: Uuid::now_v7(),
            document_id: document.id,
            requester_id: caller.actor.clone(),
            tenant_id: Some(caller.tenant_id.clone()),
            reason,
            status: "pending".to_string(),
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state.access_requests.insert(request.id, request.clone());
        state.log_access(document.id, &caller.actor, ACTION_REQUEST, None);
        state.emit(
            "vault.access.requested",
            json!({
                "document_id": document.id.to_string(),
                "request_id": request.id.to_string(),
                "requester_id": caller.actor,
                "owner_id": document.owner_id,
            }),
        );
        state.notify(
            "vault.access.requested",
            &document.owner_id,
            "Vault access requested",
            &format!(
                "{} requested access to document \"{}\"",
                caller.actor, document.name
            ),
            Some(format!("/api/v1/vault/access-requests/{}", request.id)),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_access_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let document_filter = params.filters.get("document_id").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultAccessRequest> = state
            .access_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                // Tenant first: requests are scoped to the caller's
                // tenant, managers included.
                if request.tenant_id.as_deref() != Some(caller.tenant_id.as_str()) {
                    return false;
                }
                // Managers see every request; anyone else sees only
                // their own requests and requests for documents they
                // own.
                if !is_manager {
                    let own_request = request.requester_id == caller.actor;
                    let own_document = state
                        .documents
                        .get(&request.document_id)
                        .map(|entry| entry.value().owner_id == caller.actor)
                        .unwrap_or(false);
                    if !own_request && !own_document {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &request.status != status {
                        return false;
                    }
                }
                if let Some(document_id) = &document_filter {
                    if request.document_id.to_string() != *document_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn decide_access_request(
    ctx: &AppContext,
    state: &VaultState,
    caller: &Caller,
    raw_id: &str,
    approve: bool,
    reason: Option<String>,
) -> UnifiedResult<VaultAccessRequest> {
    require_permission(ctx, &caller.role, PERM_VAULT_MANAGE)?;
    let id = parse_uuid(raw_id, "access request")?;
    let request = state
        .access_requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Access request not found: {id}")))?;
    if request.status != "pending" {
        return Err(UnifiedError::conflict(format!(
            "Only a pending access request can be decided (status is \"{}\")",
            request.status
        )));
    }
    // Tenant scope: a request from another tenant answers 404,
    // the same as a missing one.
    if request.tenant_id.as_deref() != Some(caller.tenant_id.as_str()) {
        return Err(UnifiedError::not_found(format!(
            "Access request not found: {id}"
        )));
    }
    let document = state
        .documents
        .get(&request.document_id)
        .map(|entry| entry.value().clone())
        .filter(|document| document.tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Document not found: {}", request.document_id))
        })?;
    // Only the document owner or a scoped admin for the
    // document's company may decide a request for it: holding
    // vault.manage alone is not enough. The request itself is
    // tenant-scoped (a foreign tenant's request answers 404
    // above), so this check only ever sees same-tenant rows.
    let is_manager = check_capability(ctx, &caller.role, PERM_VAULT_MANAGE);
    require_document_owner_or_scoped_admin(&document, caller, is_manager)?;
    let now = Utc::now();
    let mut updated = request.clone();
    updated.status = if approve {
        "approved".to_string()
    } else {
        "denied".to_string()
    };
    updated.decided_by = Some(caller.actor.clone());
    updated.decided_at = Some(now);
    updated.updated_at = now;
    state.access_requests.insert(updated.id, updated.clone());
    let action = if approve { ACTION_APPROVE } else { ACTION_DENY };
    state.log_access(document.id, &caller.actor, action, reason.clone());
    state.emit(
        if approve {
            "vault.access.granted"
        } else {
            "vault.access.denied"
        },
        json!({
            "document_id": document.id.to_string(),
            "request_id": request.id.to_string(),
            "requester_id": request.requester_id,
            "decided_by": caller.actor,
        }),
    );
    state.notify(
        if approve {
            "vault.access.granted"
        } else {
            "vault.access.denied"
        },
        &request.requester_id,
        if approve {
            "Vault access granted"
        } else {
            "Vault access denied"
        },
        &format!(
            "Your access request for document \"{}\" was {}",
            document.name,
            if approve { "approved" } else { "denied" }
        ),
        Some(format!("/api/v1/vault/documents/{}", document.id)),
    );
    Ok(updated)
}

pub(crate) async fn approve_access_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        decide_access_request(&ctx, &state, &caller, &path.into_inner(), true, reason)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn deny_access_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        decide_access_request(&ctx, &state, &caller, &path.into_inner(), false, reason)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

// Retention handlers

pub(crate) async fn flag_document_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let now = Utc::now();
        let mut updated = document.clone();
        updated.flagged_for_review = true;
        updated.flagged_at = Some(now);
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        state.log_access(document.id, &caller.actor, ACTION_FLAG_REVIEW, None);
        state.emit(
            "vault.document.flagged",
            json!({
                "document_id": document.id.to_string(),
                "flagged_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn retention_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                if !can_see_document(
                    &state,
                    document,
                    &caller.actor,
                    is_manager,
                    &caller.tenant_id,
                ) {
                    return false;
                }
                // Flagged documents always show up; otherwise a
                // document qualifies when its folder has a retention
                // rule and the document is older than that many days.
                if document.flagged_for_review {
                    return true;
                }
                let Some(folder) = state.folders.get(&document.folder_id) else {
                    return false;
                };
                let Some(days) = folder.value().retention_days else {
                    return false;
                };
                let age_days = (now - document.created_at).num_days();
                age_days >= days
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
