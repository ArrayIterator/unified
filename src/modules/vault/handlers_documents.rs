//! vault module - HTTP handlers for documents and versions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::access::allocate_version_number;
use super::access::can_see_document;
use super::access::enforce_version_cap;
use super::access::load_managed_document;
use super::access::load_visible_document;
use super::access::versions_of;
use super::constants::ACTION_DELETE;
use super::constants::ACTION_DOWNLOAD;
use super::constants::ACTION_RESTORE;
use super::constants::ACTION_UPLOAD;
use super::constants::ACTION_VIEW;
use super::constants::MAX_DESCRIPTION_LEN;
use super::constants::MAX_MIME_TYPE_LEN;
use super::constants::MAX_NAME_LEN;
use super::constants::MAX_NOTE_LEN;
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
use super::state::VaultState;
use super::types::CreateDocumentRequest;
use super::types::UpdateDocumentRequest;
use super::types::UploadVersionRequest;
use super::types::VaultAccessLog;
use super::types::VaultDocument;
use super::types::VaultDocumentVersion;
use super::validation::valid_checksum;
use super::validation::validate_client_ids;
use super::validation::validate_storage_ref;

// Document handlers

pub(crate) async fn list_documents(
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
        let folder_filter = params.filters.get("folder_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let flagged_filter = params.filters.get("flagged_for_review").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                // Tenant first (inside can_see_document): a caller
                // only ever sees their own tenant's documents. A
                // client only ever sees documents shared with them;
                // managers and owners see everything in scope.
                if !can_see_document(
                    &state,
                    document,
                    &caller.actor,
                    is_manager,
                    &caller.tenant_id,
                ) {
                    return false;
                }
                if let Some(folder_id) = &folder_filter {
                    if document.folder_id.to_string() != *folder_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &document.status != status {
                        return false;
                    }
                }
                // Documents carry only the resolved company of the
                // tenant that created them, so a company filter for
                // any other company matches nothing by construction
                // - the client-supplied value can never widen scope.
                if let Some(company) = &company_filter {
                    if document.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(flagged) = &flagged_filter {
                    let expected = match flagged.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if document.flagged_for_review != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !document.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
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

pub(crate) async fn create_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    body: web::Json<CreateDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let input = body.into_inner();
        let folder = state
            .folders
            .get(&input.folder_id)
            .map(|entry| entry.value().clone())
            .filter(|folder| folder.tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Folder not found: {}", input.folder_id))
            })?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        validate_storage_ref(&input.storage_target, &input.storage_key)?;
        let checksum = input.checksum_sha256.trim().to_lowercase();
        if !valid_checksum(&checksum) {
            return Err(UnifiedError::bad_request(
                "checksum_sha256 must be 64 lowercase hex characters",
            ));
        }
        if input.size_bytes < 0 {
            return Err(UnifiedError::bad_request("size_bytes must be >= 0"));
        }
        let mime_type = match &input.mime_type {
            Some(raw) => require_text(raw, "mime_type", MAX_MIME_TYPE_LEN)?,
            None => "application/octet-stream".to_string(),
        };
        let shared_with = validate_client_ids(&input.shared_with_client_ids)?;
        let now = Utc::now();
        let version = VaultDocumentVersion {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            version_number: 1,
            storage_target: input.storage_target.trim().to_string(),
            storage_key: input.storage_key.trim().to_string(),
            checksum_sha256: checksum.clone(),
            size_bytes: input.size_bytes,
            mime_type: mime_type.clone(),
            uploaded_by: caller.actor.clone(),
            note: None,
            created_at: now,
        };
        let document = VaultDocument {
            id: version.document_id,
            folder_id: folder.id,
            name,
            description,
            owner_id: caller.actor.clone(),
            // Tenant and company are bound server-side from the
            // resolved tenant context. The request body's
            // company_id is ignored (see CreateDocumentRequest):
            // a client must not be able to create a document
            // under another tenant or company.
            tenant_id: Some(caller.tenant_id.clone()),
            company_id: caller.company_id.clone(),
            current_version_id: Some(version.id),
            storage_target: version.storage_target.clone(),
            storage_key: version.storage_key.clone(),
            checksum_sha256: checksum,
            size_bytes: input.size_bytes,
            mime_type,
            shared_with_client_ids: shared_with,
            status: "active".to_string(),
            flagged_for_review: false,
            flagged_at: None,
            created_at: now,
            updated_at: now,
        };
        state.versions.insert(version.id, version);
        state.documents.insert(document.id, document.clone());
        state.log_access(document.id, &caller.actor, ACTION_UPLOAD, None);
        state.emit(
            "vault.document.uploaded",
            json!({
                "document_id": document.id.to_string(),
                "folder_id": document.folder_id.to_string(),
                "owner_id": document.owner_id,
                "version": 1,
                "size_bytes": document.size_bytes,
            }),
        );
        Ok(document)
    })();
    match result {
        Ok(document) => created(document),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        state.log_access(document.id, &caller.actor, ACTION_VIEW, None);
        Ok(document)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        let mut updated = document.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(folder_id) = input.folder_id {
            let same_tenant = state
                .folders
                .get(&folder_id)
                .map(|entry| entry.value().tenant_id.as_deref() == Some(caller.tenant_id.as_str()))
                .unwrap_or(false);
            if !same_tenant {
                return Err(UnifiedError::not_found(format!(
                    "Folder not found: {folder_id}"
                )));
            }
            updated.folder_id = folder_id;
        }
        if let Some(status) = input.status {
            match status.as_str() {
                "active" | "archived" => updated.status = status,
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "status must be \"active\" or \"archived\", got \"{other}\""
                    )));
                }
            }
        }
        if let Some(shared_with) = input.shared_with_client_ids {
            updated.shared_with_client_ids = validate_client_ids(&shared_with)?;
        }
        updated.updated_at = Utc::now();
        state.documents.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        // Deletion is always explicit and audited: the access log
        // keeps the delete entry, and the file bytes themselves are
        // the storage module's concern (this module only drops its
        // metadata rows).
        state.log_access(
            document.id,
            &caller.actor,
            ACTION_DELETE,
            Some(format!("document \"{}\" deleted", document.name)),
        );
        for version in versions_of(&state, document.id) {
            state.versions.remove(&version.id);
        }
        for link in state
            .share_links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| link.document_id == document.id)
        {
            // Removes the row and its token-hash index entry.
            state.remove_share_link(link.id);
        }
        // Pending access requests must not survive the document:
        // a request for a deleted document can never be decided,
        // and leaving it behind would keep the requester visible
        // in listings for a document that no longer exists.
        for request in state
            .access_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| request.document_id == document.id)
        {
            state.access_requests.remove(&request.id);
        }
        state.documents.remove(&document.id);
        state.emit(
            "vault.document.deleted",
            json!({
                "document_id": document.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "document_id": document.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Version handlers

pub(crate) async fn upload_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UploadVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocumentVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        validate_storage_ref(&input.storage_target, &input.storage_key)?;
        let checksum = input.checksum_sha256.trim().to_lowercase();
        if !valid_checksum(&checksum) {
            return Err(UnifiedError::bad_request(
                "checksum_sha256 must be 64 lowercase hex characters",
            ));
        }
        if input.size_bytes < 0 {
            return Err(UnifiedError::bad_request("size_bytes must be >= 0"));
        }
        let mime_type = match &input.mime_type {
            Some(raw) => require_text(raw, "mime_type", MAX_MIME_TYPE_LEN)?,
            None => document.mime_type.clone(),
        };
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_NOTE_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let version = VaultDocumentVersion {
            id: Uuid::now_v7(),
            document_id: document.id,
            // Allocated atomically per document (see
            // allocate_version_number): concurrent uploads can
            // never receive the same version number.
            version_number: allocate_version_number(&state, document.id),
            storage_target: input.storage_target.trim().to_string(),
            storage_key: input.storage_key.trim().to_string(),
            checksum_sha256: checksum.clone(),
            size_bytes: input.size_bytes,
            mime_type: mime_type.clone(),
            uploaded_by: caller.actor.clone(),
            note,
            created_at: now,
        };
        state.versions.insert(version.id, version.clone());
        let mut updated = document.clone();
        updated.current_version_id = Some(version.id);
        updated.storage_target = version.storage_target.clone();
        updated.storage_key = version.storage_key.clone();
        updated.checksum_sha256 = checksum;
        updated.size_bytes = input.size_bytes;
        updated.mime_type = mime_type;
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        enforce_version_cap(&state, &updated);
        state.log_access(document.id, &caller.actor, ACTION_UPLOAD, None);
        state.emit(
            "vault.document.versioned",
            json!({
                "document_id": document.id.to_string(),
                "version_id": version.id.to_string(),
                "version": version.version_number,
                "uploaded_by": caller.actor,
            }),
        );
        Ok(version)
    })();
    match result {
        Ok(version) => created(version),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_versions(
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
        let items = versions_of(&state, document.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn restore_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let (document_id, version_id) = path.into_inner();
        let document = load_managed_document(&ctx, &state, &document_id, &caller)?;
        let version_uuid = parse_uuid(&version_id, "version")?;
        let version = state
            .versions
            .get(&version_uuid)
            .map(|entry| entry.value().clone())
            .filter(|version| version.document_id == document.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Version not found: {version_id}")))?;
        // Restore points the document back at the old version's
        // storage reference; it does not delete the newer versions.
        let now = Utc::now();
        let mut updated = document.clone();
        updated.current_version_id = Some(version.id);
        updated.storage_target = version.storage_target.clone();
        updated.storage_key = version.storage_key.clone();
        updated.checksum_sha256 = version.checksum_sha256.clone();
        updated.size_bytes = version.size_bytes;
        updated.mime_type = version.mime_type.clone();
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        state.log_access(
            document.id,
            &caller.actor,
            ACTION_RESTORE,
            Some(format!("restored version {}", version.version_number)),
        );
        state.emit(
            "vault.document.restored",
            json!({
                "document_id": document.id.to_string(),
                "version_id": version.id.to_string(),
                "version": version.version_number,
                "restored_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

// Download + access-log handlers

pub(crate) async fn download_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        if document.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active document can be downloaded (status is \"{}\")",
                document.status
            )));
        }
        // Every download is logged (actor, action, timestamp) and
        // the file bytes are served by the storage module: the
        // response carries the storage reference only.
        state.log_access(document.id, &caller.actor, ACTION_DOWNLOAD, None);
        Ok(json!({
            "document_id": document.id.to_string(),
            "name": document.name,
            "storage_target": document.storage_target,
            "storage_key": document.storage_key,
            "checksum_sha256": document.checksum_sha256,
            "size_bytes": document.size_bytes,
            "mime_type": document.mime_type,
            "version_id": document.current_version_id.map(|id| id.to_string()),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_access_log(
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
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor_id").cloned();
        let mut items: Vec<VaultAccessLog> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.document_id != document.id {
                    return false;
                }
                if let Some(action) = &action_filter {
                    if &row.action != action {
                        return false;
                    }
                }
                if let Some(actor) = &actor_filter {
                    if &row.actor_id != actor {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
